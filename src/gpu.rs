use std::sync::Arc;

use eframe::egui_wgpu::{self, SurfaceErrorAction, WgpuConfiguration};
use eframe::wgpu::{CurrentSurfaceTexture, DeviceLostReason};

pub(crate) fn wgpu_options() -> WgpuConfiguration {
    WgpuConfiguration {
        on_surface_status: Arc::new(on_surface_status),
        ..Default::default()
    }
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
