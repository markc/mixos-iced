use crate::capture::session::message::CaptureMessage;

// The screen-capture panel is the one iced producer of surface messages.

#[derive(Debug)]
pub struct SurfaceMessage {
    // SurfaceID // <-- by HandlerID is available just in case its needed
    pub message: SurfaceMessageType
}

#[derive(Debug)]
pub enum SurfaceMessageType {
    Capture(CaptureMessage),
}
