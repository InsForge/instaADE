//! Turns API `Machine` records into the view the UI shows, and builds the size and sleep choices.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::ade::Ade;
use crate::api::types::{Machine, MachineSpec, Me, Template};

/// The server's idle time when a machine sets none, and the free plan's fixed one.
const DEFAULT_SLEEP_SECONDS: i64 = 1800;
/// The metadata key that marks a sandbox as connected. Its value lists the ids of the ADEs
/// set up on it, comma separated, in the order they were added: `t3`, `t3,cmux`. Sandboxes
/// connected by v0.1.x hold `t3`, which reads as before.
pub(crate) const BRIDGE_KEY: &str = "instabox-bridge";

/// The ids in a mark, trimmed, without empty entries.
fn mark_ids(value: &str) -> impl Iterator<Item = &str> {
    value.split(',').map(str::trim).filter(|id| !id.is_empty())
}

/// The ADEs a sandbox is marked with, in the order they were added, each once. Ids this
/// build does not know (written by a newer one) are skipped. Empty without a mark.
pub fn marked_ades(metadata: &BTreeMap<String, String>) -> Vec<Ade> {
    let mut out = Vec::new();
    for ade in metadata
        .get(BRIDGE_KEY)
        .into_iter()
        .flat_map(|v| mark_ids(v))
        .filter_map(Ade::from_id)
    {
        if !out.contains(&ade) {
            out.push(ade);
        }
    }
    out
}

/// Adds `ade` to a sandbox's mark. Every id already there stays, ones this build does not
/// know too, and so do the other keys. A first mark is the ADE's id alone, as v0.1.x wrote.
pub fn mark(metadata: &mut BTreeMap<String, String>, ade: Ade) {
    let mut ids: Vec<String> = metadata
        .get(BRIDGE_KEY)
        .map(|v| mark_ids(v).map(str::to_string).collect())
        .unwrap_or_default();
    if !ids.iter().any(|id| id == ade.id()) {
        ids.push(ade.id().to_string());
    }
    metadata.insert(BRIDGE_KEY.to_string(), ids.join(","));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ViewState {
    Creating,
    Running,
    Paused,
    Stopped,
    Waking,
    Pausing,
    Error,
    Deleting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoSleep {
    /// None when the machine has no auto-sleep value set.
    pub seconds: Option<i64>,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SleepChoice {
    pub seconds: i64,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeChoice {
    pub vcpus: u32,
    #[serde(rename = "memMiB")]
    pub mem_mib: u32,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxView {
    pub id: String,
    pub name: String,
    pub state: ViewState,
    pub size: String,
    pub connected: bool,
    /// The ADEs the sandbox is marked with, in the order they were added.
    pub ades: Vec<Ade>,
    pub t3_version: Option<String>,
    pub free: bool,
    pub auto_sleep: AutoSleep,
    pub run_left_minutes: Option<i64>,
    pub cooldown_minutes: Option<i64>,
    /// The sandbox could not be brought to the desktop T3 version (busy, ahead, or a failed sync).
    pub update_pending: bool,
    /// The sandbox's T3 is newer than the desktop's: T3 Code on this computer is behind.
    pub desktop_behind: bool,
}

pub fn view_state(state: &str) -> ViewState {
    match state {
        "creating" | "starting" => ViewState::Creating,
        "running" => ViewState::Running,
        "suspending" => ViewState::Pausing,
        "suspended" => ViewState::Paused,
        "resuming" => ViewState::Waking,
        "stopping" | "stopped" => ViewState::Stopped,
        "deleting" | "deleted" => ViewState::Deleting,
        _ => ViewState::Error,
    }
}

pub fn size_label(spec: &MachineSpec) -> String {
    let mem = if spec.mem_mib.is_multiple_of(1024) {
        format!("{}", spec.mem_mib / 1024)
    } else {
        format!("{:.1}", f64::from(spec.mem_mib) / 1024.0)
    };
    format!("{} vCPU / {} GB", spec.vcpus, mem)
}

/// The text the UI shows for an auto sleep value: "After 30 min", or "Never" for 0.
pub fn sleep_label(seconds: i64) -> String {
    if seconds == 0 {
        "Never".to_string()
    } else if seconds % 60 == 0 {
        format!("After {} min", seconds / 60)
    } else {
        format!("After {seconds} s")
    }
}

fn ceil_minutes(secs: i64) -> i64 {
    (secs + 59).div_euclid(60)
}

pub fn view(m: &Machine, now: DateTime<Utc>) -> SandboxView {
    let state = view_state(&m.state);
    let run_left_minutes = match m.run_started_at {
        Some(start) if m.free && m.max_run_minutes > 0 && state == ViewState::Running => {
            let end = start + chrono::Duration::minutes(m.max_run_minutes);
            Some(ceil_minutes((end - now).num_seconds()).max(0))
        }
        _ => None,
    };
    let cooldown_minutes = m
        .cooldown_until
        .filter(|until| *until > now)
        .map(|until| ceil_minutes((until - now).num_seconds()));
    let auto_sleep = if m.free {
        AutoSleep {
            seconds: Some(DEFAULT_SLEEP_SECONDS),
            label: sleep_label(DEFAULT_SLEEP_SECONDS),
        }
    } else {
        AutoSleep {
            seconds: m.auto_suspend_after_seconds,
            label: m.auto_suspend_after_seconds.map_or_else(
                || format!("{} (default)", sleep_label(DEFAULT_SLEEP_SECONDS)),
                sleep_label,
            ),
        }
    };
    SandboxView {
        id: m.id.clone(),
        name: m.name.clone(),
        state,
        size: size_label(&m.spec),
        connected: !marked_ades(&m.metadata).is_empty(),
        ades: marked_ades(&m.metadata),
        t3_version: None,
        free: m.free,
        auto_sleep,
        run_left_minutes,
        cooldown_minutes,
        update_pending: false,
        desktop_behind: false,
    }
}

/// Linux sandboxes only: no `os` and no `pool`.
pub fn is_linux_template(t: &Template) -> bool {
    t.os.is_none() && t.pool.is_none()
}

pub fn size_choices(specs: &[MachineSpec]) -> Vec<SizeChoice> {
    specs
        .iter()
        .map(|s| SizeChoice {
            vcpus: s.vcpus,
            mem_mib: s.mem_mib,
            label: size_label(s),
        })
        .collect()
}

pub fn sleep_choices(me: &Me, free: bool) -> Vec<SleepChoice> {
    let choice = |seconds| SleepChoice {
        seconds,
        label: sleep_label(seconds),
    };
    if free {
        return vec![choice(DEFAULT_SLEEP_SECONDS)];
    }
    let mut out: Vec<SleepChoice> = if me.idle_choices_seconds.is_empty() {
        (300..=1800).step_by(300).map(choice).collect()
    } else {
        me.idle_choices_seconds
            .iter()
            .copied()
            .map(choice)
            .collect()
    };
    if me.always_on_allowed && !out.iter().any(|c| c.seconds == 0) {
        out.push(choice(0));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::TemplateRef;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap()
    }

    fn machine(state: &str) -> Machine {
        Machine {
            id: "m1".into(),
            name: "box".into(),
            state: state.into(),
            template: TemplateRef { id: "t".into() },
            spec: MachineSpec {
                vcpus: 4,
                mem_mib: 8192,
            },
            auto_suspend_after_seconds: Some(600),
            max_run_minutes: -1,
            run_started_at: None,
            cooldown_until: None,
            metadata: Default::default(),
            org_id: String::new(),
            free: false,
            operation_id: None,
        }
    }

    fn me(idle: Vec<i64>, always_on: bool) -> Me {
        Me {
            user_id: 1,
            login: "a".into(),
            allowed_specs: vec![],
            org: None,
            fixed_auto_suspend_seconds: None,
            idle_choices_seconds: idle,
            always_on_allowed: always_on,
        }
    }

    fn secs(v: &[SleepChoice]) -> Vec<i64> {
        v.iter().map(|c| c.seconds).collect()
    }

    #[test]
    fn state_mapping() {
        let table = [
            ("creating", ViewState::Creating),
            ("starting", ViewState::Creating),
            ("running", ViewState::Running),
            ("suspending", ViewState::Pausing),
            ("suspended", ViewState::Paused),
            ("resuming", ViewState::Waking),
            ("stopping", ViewState::Stopped),
            ("stopped", ViewState::Stopped),
            ("deleting", ViewState::Deleting),
            ("deleted", ViewState::Deleting),
            ("error", ViewState::Error),
        ];
        for (s, want) in table {
            assert_eq!(view(&machine(s), now()).state, want, "{s}");
        }
    }

    #[test]
    fn state_serializes_lowercase_and_view_camel_case() {
        let v = view(&machine("suspended"), now());
        let j = serde_json::to_value(&v).unwrap();
        assert_eq!(j["state"], "paused");
        assert_eq!(j["desktopBehind"], false);
        assert!(j.get("runLeftMinutes").is_some());
    }

    #[test]
    fn size_label_format() {
        assert_eq!(view(&machine("running"), now()).size, "4 vCPU / 8 GB");
        let s = |mem_mib| size_label(&MachineSpec { vcpus: 2, mem_mib });
        assert_eq!(s(4096), "2 vCPU / 4 GB");
        assert_eq!(s(1536), "2 vCPU / 1.5 GB");
    }

    #[test]
    fn connected_from_metadata() {
        let mut m = machine("running");
        assert!(!view(&m, now()).connected);
        m.metadata.insert("instabox-bridge".into(), "other".into());
        assert!(!view(&m, now()).connected);
        m.metadata.insert("instabox-bridge".into(), "t3".into());
        let v = view(&m, now());
        assert!(v.connected);
        assert_eq!(v.t3_version, None);
        assert_eq!(v.ades, [Ade::T3]);
        m.metadata
            .insert("instabox-bridge".into(), "t3,cmux".into());
        assert_eq!(view(&m, now()).ades, [Ade::T3, Ade::Cmux]);
    }

    fn marked(value: &str) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("a".to_string(), "1".to_string()),
            ("instabox-bridge".to_string(), value.to_string()),
        ])
    }

    #[test]
    fn the_mark_holds_the_ade_id_and_keeps_other_keys() {
        let mut md = BTreeMap::from([("a".to_string(), "1".to_string())]);
        assert_eq!(marked_ades(&md), []);
        mark(&mut md, Ade::T3);
        // The same mark v0.1.x wrote, so its sandboxes stay connected.
        assert_eq!(md, marked("t3"));
        assert_eq!(marked_ades(&md), [Ade::T3]);
        md.insert("instabox-bridge".into(), "other".into());
        assert_eq!(marked_ades(&md), []);
        let j = serde_json::to_value(view(&machine("running"), now())).unwrap();
        assert_eq!(j["ades"], serde_json::json!([]));
    }

    #[test]
    fn a_v0_1_mark_reads_exactly_as_before() {
        // v0.1.x wrote the bare id; it is T3 and only T3, and adding T3 again changes nothing.
        let mut md = marked("t3");
        assert_eq!(marked_ades(&md), [Ade::T3]);
        mark(&mut md, Ade::T3);
        assert_eq!(md, marked("t3"));
    }

    #[test]
    fn the_mark_holds_several_ades_in_the_order_they_came() {
        let mut md = marked("t3");
        mark(&mut md, Ade::Cmux);
        assert_eq!(md, marked("t3,cmux"));
        assert_eq!(marked_ades(&md), [Ade::T3, Ade::Cmux]);
        mark(&mut md, Ade::T3);
        assert_eq!(md, marked("t3,cmux"), "each id once");

        let mut md = BTreeMap::new();
        mark(&mut md, Ade::Cmux);
        mark(&mut md, Ade::T3);
        assert_eq!(md["instabox-bridge"], "cmux,t3");
        assert_eq!(marked_ades(&md), [Ade::Cmux, Ade::T3]);
    }

    #[test]
    fn the_mark_reads_loosely_and_keeps_ids_it_does_not_know() {
        assert_eq!(marked_ades(&marked(" t3 , cmux ")), [Ade::T3, Ade::Cmux]);
        assert_eq!(marked_ades(&marked("t3,,t3,")), [Ade::T3]);
        assert_eq!(marked_ades(&marked("")), []);
        // An ADE a newer build added stays in the mark when this build adds its own.
        let mut md = marked("orca,t3");
        assert_eq!(marked_ades(&md), [Ade::T3]);
        mark(&mut md, Ade::Cmux);
        assert_eq!(md, marked("orca,t3,cmux"));
    }

    #[test]
    fn free_run_left() {
        let mut m = machine("running");
        m.free = true;
        m.max_run_minutes = 60;
        m.run_started_at = Some(now() - chrono::Duration::minutes(45));
        assert_eq!(view(&m, now()).run_left_minutes, Some(15));
        m.max_run_minutes = -1;
        assert_eq!(view(&m, now()).run_left_minutes, None);
        m.max_run_minutes = 0;
        assert_eq!(view(&m, now()).run_left_minutes, None);
        m.max_run_minutes = 60;
        m.run_started_at = None;
        assert_eq!(view(&m, now()).run_left_minutes, None);
        m.run_started_at = Some(now() - chrono::Duration::minutes(45));
        m.free = false;
        assert_eq!(view(&m, now()).run_left_minutes, None);
        m.free = true;
        m.state = "suspended".into();
        assert_eq!(view(&m, now()).run_left_minutes, None);
    }

    #[test]
    fn cooldown_rounds_up_and_ignores_past() {
        let mut m = machine("suspended");
        m.cooldown_until = Some(now() + chrono::Duration::seconds(61));
        assert_eq!(view(&m, now()).cooldown_minutes, Some(2));
        m.cooldown_until = Some(now() + chrono::Duration::minutes(10));
        assert_eq!(view(&m, now()).cooldown_minutes, Some(10));
        m.cooldown_until = Some(now() - chrono::Duration::seconds(1));
        assert_eq!(view(&m, now()).cooldown_minutes, None);
        m.cooldown_until = None;
        assert_eq!(view(&m, now()).cooldown_minutes, None);
    }

    #[test]
    fn linux_template_only() {
        let t = |os: Option<&str>, pool: Option<&str>| Template {
            id: "x".into(),
            os: os.map(Into::into),
            pool: pool.map(Into::into),
            hidden: false,
            creatable: true,
            latest: true,
        };
        assert!(is_linux_template(&t(None, None)));
        assert!(!is_linux_template(&t(Some("macos"), None)));
        assert!(!is_linux_template(&t(None, Some("gpu"))));
    }

    #[test]
    fn size_choices_labels() {
        let c = size_choices(&[
            MachineSpec {
                vcpus: 2,
                mem_mib: 4096,
            },
            MachineSpec {
                vcpus: 4,
                mem_mib: 8192,
            },
        ]);
        assert_eq!(c[0].label, "2 vCPU / 4 GB");
        assert_eq!(c[1].mem_mib, 8192);
    }

    #[test]
    fn sleep_choices_free_and_paid() {
        let free = sleep_choices(&me(vec![300], true), true);
        assert_eq!(secs(&free), vec![1800]);
        assert_eq!(free[0].label, "After 30 min");

        let paid = sleep_choices(&me(vec![], false), false);
        assert_eq!(secs(&paid), vec![300, 600, 900, 1200, 1500, 1800]);
        assert_eq!(paid[0].label, "After 5 min");

        let always = sleep_choices(&me(vec![600, 1200], true), false);
        assert_eq!(secs(&always), vec![600, 1200, 0]);
        assert_eq!(always[2].label, "Never");
        assert_eq!(sleep_label(90), "After 90 s");
    }

    #[test]
    fn auto_sleep_labels_read_alike() {
        let mut m = machine("running");
        assert_eq!(view(&m, now()).auto_sleep.label, "After 10 min");
        m.auto_suspend_after_seconds = Some(0);
        assert_eq!(view(&m, now()).auto_sleep.label, "Never");
        // A paid machine with no value runs on the server's default.
        m.auto_suspend_after_seconds = None;
        let a = view(&m, now()).auto_sleep;
        assert_eq!(
            (a.seconds, a.label.as_str()),
            (None, "After 30 min (default)")
        );
        m.free = true;
        assert_eq!(view(&m, now()).auto_sleep.label, "After 30 min");
    }
}
