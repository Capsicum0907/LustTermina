use std::sync::Arc;

use eframe::egui_wgpu::{self, SurfaceErrorAction, WgpuConfiguration, WgpuSetup};
use eframe::wgpu::{Backends, CurrentSurfaceTexture, DeviceLostReason};

pub(crate) fn wgpu_options(renderer: &str) -> WgpuConfiguration {
    let mut cfg = WgpuConfiguration {
        on_surface_status: Arc::new(on_surface_status),
        ..Default::default()
    };
    if let Some(backends) = renderer_backends(renderer) {
        if let WgpuSetup::CreateNew(setup) = &mut cfg.wgpu_setup {
            setup.instance_descriptor.backends = backends;
        }
    }
    cfg
}

fn renderer_backends(renderer: &str) -> Option<Backends> {
    let renderer = renderer.trim();
    if renderer.is_empty() || renderer.eq_ignore_ascii_case("auto") {
        return None;
    }
    let backends = Backends::from_comma_list(renderer);
    if backends.is_empty() {
        log::warn!("renderer {renderer:?} is not a known backend; using the default");
        return None;
    }
    log::info!("renderer {renderer:?} requested from config");
    Some(backends)
}

fn on_surface_status(status: &CurrentSurfaceTexture) -> SurfaceErrorAction {
    match status {
        CurrentSurfaceTexture::Outdated => {
            log::debug!("gpu surface status Outdated: reconfigure");
            SurfaceErrorAction::Reconfigure
        }
        CurrentSurfaceTexture::Occluded => {
            log::debug!("gpu surface status Occluded: skip frame");
            SurfaceErrorAction::SkipFrame
        }
        CurrentSurfaceTexture::Lost => {
            log::warn!("gpu surface status Lost: recreate surface");
            SurfaceErrorAction::RecreateSurface
        }
        other => {
            log::warn!("gpu surface status {other:?}: skip frame");
            SurfaceErrorAction::SkipFrame
        }
    }
}

pub(crate) fn watch_device(rs: &egui_wgpu::RenderState) {
    rs.device.set_device_lost_callback(on_device_lost);
}

fn on_device_lost(reason: DeviceLostReason, message: String) {
    match reason {
        DeviceLostReason::Destroyed => log::info!("gpu device destroyed: {message}"),
        _ => log::error!("gpu device lost reason={reason:?}: {message}"),
    }
    log::logger().flush();
}
