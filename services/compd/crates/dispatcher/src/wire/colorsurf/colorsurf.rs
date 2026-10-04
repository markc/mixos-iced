use std::sync::Mutex;
use smithay::reexports::wayland_protocols::wp::color_management::v1::server::{
    wp_color_management_output_v1::{Request as OutReq, WpColorManagementOutputV1},
    wp_color_management_surface_feedback_v1::{Request as FbReq, WpColorManagementSurfaceFeedbackV1},
    wp_color_management_surface_v1::{Request as SurfReq, WpColorManagementSurfaceV1},
    wp_color_manager_v1::{Primaries, Request as MgrReq, TransferFunction, WpColorManagerV1},
    wp_image_description_creator_icc_v1::{Request as IccReq, WpImageDescriptionCreatorIccV1},
    wp_image_description_creator_params_v1::{Request as PrReq, WpImageDescriptionCreatorParamsV1},
    wp_image_description_info_v1::WpImageDescriptionInfoV1,
    wp_image_description_v1::{Cause, Request as DsReq, WpImageDescriptionV1},
};
use smithay::reexports::wayland_server::{DataInit, Dispatch, Resource};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use crate::wire::color::color::{ImageDescData, ParamsState, SurfaceColor, send_ready, store_surface_color, wenum};

fn srgb() -> ImageDescData {
    ImageDescData { transfer: Some(TransferFunction::Srgb), primaries: Some(Primaries::Srgb) }
}
type DI<'a, W> = DataInit<'a, W>;

pub fn dispatch_color_manager<W>(req: MgrReq, di: &mut DI<'_, W>)
where
    W: Dispatch<WpColorManagementOutputV1, ()> + 'static,
    W: Dispatch<WpColorManagementSurfaceV1, WlSurface> + 'static,
    W: Dispatch<WpColorManagementSurfaceFeedbackV1, WlSurface> + 'static,
    W: Dispatch<WpImageDescriptionCreatorIccV1, ()> + 'static,
    W: Dispatch<WpImageDescriptionCreatorParamsV1, Mutex<ParamsState>> + 'static,
    W: Dispatch<WpImageDescriptionV1, ImageDescData> + 'static,
{
    match req {
        MgrReq::Destroy => {}
        MgrReq::GetOutput { id, .. } => { di.init(id, ()); }
        MgrReq::GetSurface { id, surface } => { di.init(id, surface); }
        MgrReq::GetSurfaceFeedback { id, surface } => { di.init(id, surface); }
        MgrReq::CreateIccCreator { obj } => { di.init(obj, ()); }
        MgrReq::CreateParametricCreator { obj } => { di.init(obj, Mutex::new(ParamsState::default())); }
        MgrReq::CreateWindowsScrgb { image_description } => {
            di.init(image_description, ImageDescData::default()).failed(Cause::Unsupported, "windows scRGB not supported".into());
        }
        MgrReq::GetImageDescription { image_description, .. } => { send_ready(&di.init(image_description, srgb())); }
        _ => {}
    }
}

pub fn dispatch_color_output<W: Dispatch<WpImageDescriptionV1, ImageDescData> + 'static>(req: OutReq, di: &mut DI<'_, W>) {
    if let OutReq::GetImageDescription { image_description } = req { send_ready(&di.init(image_description, srgb())); }
}

pub fn dispatch_color_surface(req: SurfReq, surface: &WlSurface) {
    match req {
        SurfReq::SetImageDescription { image_description, .. } => {
            if let Some(d) = image_description.data::<ImageDescData>() {
                store_surface_color(surface, Some(SurfaceColor { transfer: d.transfer, primaries: d.primaries }));
            }
        }
        SurfReq::UnsetImageDescription | SurfReq::Destroy => { store_surface_color(surface, None); }
        _ => {}
    }
}

pub fn dispatch_color_feedback<W: Dispatch<WpImageDescriptionV1, ImageDescData> + 'static>(req: FbReq, di: &mut DI<'_, W>) {
    match req {
        FbReq::GetPreferred { image_description } | FbReq::GetPreferredParametric { image_description } => {
            send_ready(&di.init(image_description, srgb()));
        }
        _ => {}
    }
}

pub fn dispatch_color_params<W: Dispatch<WpImageDescriptionV1, ImageDescData> + 'static>(req: PrReq, data: &Mutex<ParamsState>, di: &mut DI<'_, W>) {
    match req {
        PrReq::SetTfNamed { tf } => { data.lock().unwrap().transfer = wenum(tf); }
        PrReq::SetPrimariesNamed { primaries } => { data.lock().unwrap().primaries = wenum(primaries); }
        PrReq::Create { image_description } => {
            let p = data.lock().unwrap();
            send_ready(&di.init(image_description, ImageDescData { transfer: p.transfer, primaries: p.primaries }));
        }
        _ => {}
    }
}

pub fn dispatch_color_icc<W: Dispatch<WpImageDescriptionV1, ImageDescData> + 'static>(req: IccReq, di: &mut DI<'_, W>) {
    if let IccReq::Create { image_description } = req {
        di.init(image_description, ImageDescData::default()).failed(Cause::Unsupported, "ICC profiles not supported".into());
    }
}

pub fn dispatch_image_desc<W: Dispatch<WpImageDescriptionInfoV1, ()> + 'static>(req: DsReq, data: &ImageDescData, di: &mut DI<'_, W>) {
    if let DsReq::GetInformation { information } = req {
        let info: WpImageDescriptionInfoV1 = di.init(information, ());
        if let Some(p) = data.primaries { info.primaries_named(p); }
        if let Some(tf) = data.transfer { info.tf_named(tf); }
        info.done();
    }
}
