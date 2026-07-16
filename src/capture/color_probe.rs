// wp-color-management-v1 readout for the --wayland-diag hdr section: what
// transfer function, primaries, and luminance range each output runs at.
// this is a readiness detector, not a capture path — as of mid-2026 no
// compositor hands capture clients hdr pixels (kwin's ScreenShot2 is
// sdr-only and kde rejected ext-image-copy outright, bug 513785), so the
// verdict this feeds tells a user with an hdr desktop why captures are sdr
// and will flip the moment a compositor starts offering >8-bit buffers.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use wayland_client::backend::ObjectId;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::color_management::v1::client::{
    wp_color_management_output_v1, wp_color_manager_v1, wp_image_description_info_v1,
    wp_image_description_v1,
};

#[derive(Debug, Default, Clone)]
pub struct OutputColorInfo {
    pub output: String,
    pub transfer: Option<String>,
    pub primaries: Option<String>,
    // (min, max, reference) in cd/m2
    pub luminance: Option<(f64, f64, f64)>,
    pub target_max_cll: Option<f64>,
    pub failed: Option<String>,
}

impl OutputColorInfo {
    // a pq or extended-linear signal with above-sdr luminance is what an hdr
    // capture source would have to preserve
    pub fn is_hdr_signal(&self) -> bool {
        matches!(self.transfer.as_deref(), Some("st2084" | "ext_linear"))
    }
}

#[derive(Default)]
struct DescriptionState {
    settled: bool,
    failed: Option<String>,
}

#[derive(Default)]
struct InfoState {
    done: bool,
    transfer: Option<String>,
    primaries: Option<String>,
    luminance: Option<(f64, f64, f64)>,
    target_max_cll: Option<f64>,
}

#[derive(Default)]
struct State {
    outputs: Vec<(wl_output::WlOutput, Option<String>)>,
    descriptions: HashMap<ObjectId, DescriptionState>,
    infos: HashMap<ObjectId, InfoState>,
}

pub fn probe_outputs() -> Result<Vec<OutputColorInfo>> {
    let connection = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<State>(&connection)?;
    let qh = queue.handle();
    let manager = globals
        .bind::<wp_color_manager_v1::WpColorManagerV1, _, _>(&qh, 1..=2, ())
        .map_err(|_| anyhow!("compositor doesn't expose wp_color_manager_v1"))?;

    let mut state = State::default();
    let registry = globals.registry();
    globals.contents().with_list(|list| {
        for global in list {
            if global.interface == "wl_output" && global.version >= 4 {
                let output = registry.bind::<wl_output::WlOutput, _, _>(global.name, 4, &qh, ());
                state.outputs.push((output, None));
            }
        }
    });
    for _ in 0..3 {
        queue.roundtrip(&mut state)?;
        if state.outputs.iter().all(|(_, name)| name.is_some()) {
            break;
        }
    }

    let mut results = Vec::new();
    let outputs = state.outputs.clone();
    for (output, name) in outputs {
        let mut info = OutputColorInfo {
            output: name.unwrap_or_else(|| "unnamed".to_string()),
            ..Default::default()
        };
        let color_output = manager.get_output(&output, &qh, ());
        let description = color_output.get_image_description(&qh, ());
        let description_id = description.id();
        state
            .descriptions
            .insert(description_id.clone(), DescriptionState::default());
        for _ in 0..8 {
            queue.roundtrip(&mut state)?;
            if state.descriptions[&description_id].settled {
                break;
            }
        }
        let settled = state.descriptions.remove(&description_id).unwrap_or_default();
        if let Some(cause) = settled.failed {
            info.failed = Some(cause);
        } else if settled.settled {
            // output-created descriptions always allow get_information
            let details = description.get_information(&qh, ());
            let details_id = details.id();
            state.infos.insert(details_id.clone(), InfoState::default());
            for _ in 0..8 {
                queue.roundtrip(&mut state)?;
                if state.infos[&details_id].done {
                    break;
                }
            }
            let collected = state.infos.remove(&details_id).unwrap_or_default();
            info.transfer = collected.transfer;
            info.primaries = collected.primaries;
            info.luminance = collected.luminance;
            info.target_max_cll = collected.target_max_cll;
        } else {
            info.failed = Some("image description never settled".to_string());
        }
        description.destroy();
        color_output.destroy();
        results.push(info);
    }
    Ok(results)
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            if let Some((_, slot)) = state.outputs.iter_mut().find(|(o, _)| o == output) {
                *slot = Some(name);
            }
        }
    }
}

impl Dispatch<wp_image_description_v1::WpImageDescriptionV1, ()> for State {
    fn event(
        state: &mut Self,
        description: &wp_image_description_v1::WpImageDescriptionV1,
        event: wp_image_description_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(entry) = state.descriptions.get_mut(&description.id()) else {
            return;
        };
        match event {
            wp_image_description_v1::Event::Ready { .. }
            | wp_image_description_v1::Event::Ready2 { .. } => entry.settled = true,
            wp_image_description_v1::Event::Failed { cause, msg } => {
                entry.settled = true;
                entry.failed = Some(format!("{cause:?}: {msg}"));
            }
            _ => {}
        }
    }
}

impl Dispatch<wp_image_description_info_v1::WpImageDescriptionInfoV1, ()> for State {
    fn event(
        state: &mut Self,
        details: &wp_image_description_info_v1::WpImageDescriptionInfoV1,
        event: wp_image_description_info_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wp_image_description_info_v1::Event;
        let Some(entry) = state.infos.get_mut(&details.id()) else {
            return;
        };
        match event {
            Event::TfNamed {
                tf: WEnum::Value(tf),
            } => entry.transfer = Some(format!("{tf:?}").to_ascii_lowercase()),
            Event::TfPower { eexp } => {
                entry.transfer = Some(format!("power({})", eexp as f64 / 10000.0))
            }
            Event::PrimariesNamed {
                primaries: WEnum::Value(primaries),
            } => entry.primaries = Some(format!("{primaries:?}").to_ascii_lowercase()),
            Event::Primaries { .. } => {
                entry.primaries.get_or_insert_with(|| "custom".to_string());
            }
            Event::Luminances {
                min_lum,
                max_lum,
                reference_lum,
            } => {
                entry.luminance = Some((
                    min_lum as f64 / 10000.0,
                    max_lum as f64,
                    reference_lum as f64,
                ));
            }
            Event::TargetMaxCll { max_cll } => entry.target_max_cll = Some(max_cll as f64),
            Event::Done => entry.done = true,
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(State: ignore wp_color_manager_v1::WpColorManagerV1);
wayland_client::delegate_noop!(State: ignore wp_color_management_output_v1::WpColorManagementOutputV1);
