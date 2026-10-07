// SPDX-License-Identifier: MIT OR Apache-2.0
//! Actual Winit surface submission, native receipts and leased credit bounds.
use softbuffer::{Context, Surface};
use std::{collections::HashMap, num::NonZeroU32, sync::Arc, time::{Duration, Instant}};
use winit::{application::ApplicationHandler, dpi::LogicalSize, event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop}, platform::wayland::{EventLoopBuilderExtWayland, WindowExtWayland},
    presentation::{PresentationError, PresentationFeedback, PresentationOutcome}, window::{Window, WindowId}};

type Fault = Box<dyn std::error::Error>;

// Field order retires the renderer's surface/context before its window owner.
struct Native {
    surface: Surface<Arc<Window>, Arc<Window>>,
    _context: Context<Arc<Window>>,
    window: Arc<Window>,
    submitted: bool,
}
impl Native {
    fn new(event_loop:&ActiveEventLoop) -> Result<Self,Fault> {
        let window = Arc::new(event_loop.create_window(Window::default_attributes()
            .with_title("Native frame ownership fixture").with_inner_size(LogicalSize::new(120.0,96.0)))?);
        let context = Context::new(window.clone())?;
        let surface = Surface::new(&context,window.clone())?;
        Ok(Self {surface,_context:context,window,submitted:false})
    }

    fn capacity(&self) -> Result<(),Fault> {
        if self.window.request_presentation_feedback() != Err(PresentationError::Capacity) {
            return Err("reservation escaped its native credit limit".into());
        }
        Ok(())
    }
}

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
enum Phase { Basic, EmptyDamage, Burst, Replacement, ChurnSetup, Churn, Recovery, Done }

struct Probe {
    slots: Vec<Option<Native>>,
    pending: HashMap<u64,WindowId>,
    held: Vec<PresentationFeedback>,
    phase: Phase,
    deadline: Instant,
    fault: Option<String>,
    presented: usize,
}

impl Probe {
    fn new() -> Self { Self {slots:Vec::new(),pending:HashMap::new(),held:Vec::new(),phase:Phase::Basic,
        deadline:Instant::now()+Duration::from_secs(30),fault:None,presented:0} }

    fn reserve(&mut self, slot:usize, count:usize) -> Result<(),Fault> {
        let native = self.slots[slot].as_ref().ok_or("retired window")?;
        for _ in 0..count {
            let id = native.window.request_presentation_feedback().map_err(|error| format!("native request: {error:?}"))?;
            if self.pending.insert(id.get(),native.window.id()).is_some() { return Err("reused request id".into()); }
        }
        if self.pending.len()>128 { return Err("fixture pending capacity exceeded".into()); }
        Ok(())
    }

    fn submit(&mut self, slot:usize) -> Result<(),Fault> {
        if self.slots[slot].as_ref().unwrap().submitted { return Ok(()); }
        let size = self.slots[slot].as_ref().unwrap().window.inner_size();
        let (Some(width),Some(height)) = (NonZeroU32::new(size.width),NonZeroU32::new(size.height)) else { return Ok(()); };
        let count = match self.phase {Phase::Basic|Phase::EmptyDamage=>1,Phase::Burst=>8,
            Phase::Replacement|Phase::ChurnSetup|Phase::Churn|Phase::Recovery=>0,Phase::Done=>return Ok(())};
        let native = self.slots[slot].as_mut().unwrap();
        native.surface.resize(width,height)?;
        let mut buffer = native.surface.buffer_mut()?;
        buffer.fill(0x003579a1);
        native.window.pre_present_notify();
        for _ in 0..count {
            let id = native.window.request_presentation_feedback().map_err(|error| format!("native request: {error:?}"))?;
            if self.pending.insert(id.get(),native.window.id()).is_some() { return Err("reused request id".into()); }
        }
        if self.phase==Phase::Burst && native.window.request_presentation_feedback()!=Err(PresentationError::Capacity) {
            return Err("burst escaped window cap before submission".into());
        }
        if self.phase==Phase::EmptyDamage { buffer.present_with_damage(&[])?; } else { buffer.present()?; }
        native.submitted = true;
        if self.phase==Phase::ChurnSetup && self.slots.iter().flatten().all(|native|native.submitted) {
            // Initial configure/map commits contain no feedback requests.
            self.phase=Phase::Churn;
            for slot in 0..16 {self.reserve(slot,8)?;}
            self.slots[16].as_ref().unwrap().capacity()?;
            for slot in 0..16 {
                let native = self.slots[slot].as_mut().unwrap();
                native.submitted=false;
                native.window.request_redraw();
            }
        }
        Ok(())
    }

    fn main_redraw(&mut self, phase:Phase) {
        self.phase = phase;
        let main = self.slots[0].as_mut().unwrap();
        main.submitted=false;
        main.window.request_redraw();
    }

    fn receipt(&mut self,event_loop:&ActiveEventLoop,window:WindowId,receipt:PresentationFeedback) -> Result<(),Fault> {
        if self.pending.remove(&receipt.id.get()) != Some(window) { return Err("foreign or duplicate terminal receipt".into()); }
        if let PresentationOutcome::Presented {nanoseconds,..} = receipt.outcome {
            if nanoseconds>=1_000_000_000 { return Err("invalid native timestamp".into()); }
            self.presented+=1;
        } else { return Err("fixture commit was discarded instead of presented".into()); }
        match self.phase {
            Phase::Basic=>{drop(receipt);self.main_redraw(Phase::EmptyDamage);}
            Phase::EmptyDamage=>{drop(receipt);self.main_redraw(Phase::Burst);}
            Phase::Burst=>{
                // The first public handler still owns its receipt; all other
                // terminal charges remain in the actual delivery path.
                self.slots[0].as_ref().unwrap().capacity()?;
                self.held.push(receipt);
                if self.pending.is_empty() {
                    if self.held.len()!=8 {return Err("burst receipt count".into());}
                    let clones:Vec<_> = self.held.to_vec();
                    self.held.clear();
                    self.slots[0].as_ref().unwrap().capacity()?;
                    self.held=clones;
                    drop(self.held.pop());
                    self.reserve(0,1)?;
                    self.slots[0].as_ref().unwrap().capacity()?;
                    self.main_redraw(Phase::Replacement);
                }
            }
            Phase::Replacement=>{
                self.slots[0].as_ref().unwrap().capacity()?;
                drop(receipt);
                self.held.clear();
                // Reserve all 128 native objects before any new commit.
                self.phase=Phase::ChurnSetup;
                for _ in 1..17 {self.slots.push(Some(Native::new(event_loop)?));}
                for slot in 0..17 {
                    let native = self.slots[slot].as_mut().unwrap();
                    native.submitted=false;
                    native.window.request_redraw();
                }
            }
            Phase::Churn=>{
                self.held.push(receipt);
                if self.pending.is_empty() {
                    if self.held.len()!=128 {return Err("process receipt count".into());}
                    self.slots[16].as_ref().unwrap().capacity()?;
                    for slot in 0..16 {drop(self.slots[slot].take());}
                    self.slots[16].as_ref().unwrap().capacity()?;
                    self.held.clear();
                    self.phase=Phase::Recovery;
                    self.reserve(16,8)?;
                    self.slots[16].as_ref().unwrap().capacity()?;
                    self.slots[16].as_mut().unwrap().submitted=false;
                    self.slots[16].as_ref().unwrap().window.request_redraw();
                }
            }
            Phase::Recovery=>{
                drop(receipt);
                if self.pending.is_empty() {self.phase=Phase::Done;event_loop.exit();}
            }
            Phase::ChurnSetup|Phase::Done=>return Err("receipt outside a submitted fixture phase".into()),
        }
        Ok(())
    }

    fn failed(&mut self,event_loop:&ActiveEventLoop,error:Fault) {
        self.fault=Some(error.to_string());event_loop.exit();
    }
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self,event_loop:&ActiveEventLoop) {
        if !self.slots.is_empty() {return;}
        match Native::new(event_loop) {
            Ok(native)=>{native.window.request_redraw();self.slots.push(Some(native));}
            Err(error)=>self.failed(event_loop,error),
        }
    }

    fn window_event(&mut self,event_loop:&ActiveEventLoop,window:WindowId,event:WindowEvent) {
        let result = match event {
            WindowEvent::PresentationFeedback(receipt)=>self.receipt(event_loop,window,receipt),
            WindowEvent::RedrawRequested=>{
                let slot = self.slots.iter().position(|native| native.as_ref().is_some_and(|native|native.window.id()==window));
                match slot {Some(16) if !matches!(self.phase,Phase::Recovery|Phase::ChurnSetup)=>Ok(()),Some(slot)=>self.submit(slot),None=>Ok(())}
            }
            WindowEvent::Resized(_)=>{
                if let Some(native) = self.slots.iter().flatten().find(|native|native.window.id()==window) {
                    if !native.submitted {native.window.request_redraw();}
                }
                Ok(())
            }
            WindowEvent::CloseRequested=>Err("fixture window closed externally".into()),
            _=>Ok(()),
        };
        if let Err(error)=result {self.failed(event_loop,error);}
    }

    fn about_to_wait(&mut self,event_loop:&ActiveEventLoop) {
        if Instant::now()>=self.deadline && self.phase!=Phase::Done {
            self.failed(event_loop,format!("native deadline at {:?}, {} pending",self.phase,self.pending.len()).into());
        } else {event_loop.set_control_flow(ControlFlow::WaitUntil(self.deadline));}
    }
}

fn main() -> Result<(),Fault> {
    let event_loop = EventLoop::builder().with_wayland().build()?;
    let mut probe = Probe::new();
    event_loop.run_app(&mut probe)?;
    if let Some(error)=probe.fault {return Err(error.into());}
    if probe.phase!=Phase::Done || !probe.pending.is_empty() || !probe.held.is_empty() {return Err("incomplete native fixture".into());}
    println!("WINIT_FRAME PASS presented={} empty_damage=true typed_delivery_credit=true clone_credit=true process_cap=128 close_retains=true recovered=true",probe.presented);
    Ok(())
}
