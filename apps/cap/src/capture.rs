// SPDX-License-Identifier: MIT OR Apache-2.0
//! A capture job runs through the native Bus; completion is a final RPC reply.
//! The compositor applies fenced minimisation before the newly requested frame.
use crate::{bus::BusHandle, document::Document};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Screen,
    Window,
    Region,
}
impl Mode {
    pub const ALL: [Self; 3] = [Self::Screen, Self::Window, Self::Region];
    pub fn key(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::Window => "window",
            Self::Region => "region",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub id: u64,
    pub generation: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub window: Option<Target>,
    #[serde(default = "yes")]
    pub cursor: bool,
    #[serde(default)]
    pub delay: u32,
}
fn yes() -> bool {
    true
}
impl Default for Request {
    fn default() -> Self {
        Self {
            mode: Mode::Screen,
            output: None,
            window: None,
            cursor: true,
            delay: 0,
        }
    }
}
impl Request {
    pub fn validate(&self) -> Result<(), String> {
        if self.delay > 10 {
            return Err("delay must be 0..10 seconds".into());
        }
        if self
            .output
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 256)
        {
            return Err("invalid output name".into());
        }
        if self.mode == Mode::Window && self.window.is_none() {
            return Err("choose a window first".into());
        }
        if self.mode != Mode::Window && self.window.is_some() {
            return Err("window only applies to window mode".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub target: Target,
    pub title: String,
    pub app_id: String,
    pub focused: bool,
    pub minimized: bool,
}
pub fn windows(value: &Value) -> Vec<Window> {
    value["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            Some(Window {
                target: Target {
                    id: row["id"].as_u64()?,
                    generation: row["generation"].as_u64()?,
                },
                title: row["title"].as_str().unwrap_or("").into(),
                app_id: row["app_id"].as_str().unwrap_or("").into(),
                focused: row["focused"].as_bool().unwrap_or(false),
                minimized: row["minimized"].as_bool().unwrap_or(false),
            })
        })
        .collect()
}
pub async fn list(bus: &BusHandle, comp: &str) -> Result<Value, String> {
    bus.call(comp, "comp.windows.list", json!({}), Duration::from_secs(5))
        .await
}

#[derive(Debug, Clone)]
pub struct Captured {
    pub document: Document,
    pub path: PathBuf,
    pub metadata: Value,
}

pub fn media_directory() -> Result<PathBuf, String> {
    let base = std::env::var_os("MIXOS_APP_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("MIXOS_APPS_HOME").map(|p| PathBuf::from(p).join("cap")))
        .or_else(|| {
            std::env::var_os("XDG_STATE_HOME").map(|p| PathBuf::from(p).join("mixos/apps/cap"))
        })
        .or_else(|| {
            std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state/mixos/apps/cap"))
        })
        .filter(|p| p.is_absolute())
        .ok_or("no absolute application state directory")?;
    let directory = base.join("captures");
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    Ok(directory)
}

/// `own` is resolved before minimising. The compositor executes its visibility
/// effect before answering `minimize`; a screenshot forces a fresh full frame.
/// Always restore a window we changed, including cancelled region selections.
pub async fn take(
    bus: BusHandle,
    comp: String,
    request: Request,
    own: Option<Target>,
    directory: PathBuf,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<Captured, String> {
    request.validate()?;
    if *cancel.borrow() {
        return Err("cancelled".into());
    }
    let mut hidden = false;
    let result = async {
        if request.delay > 0 {
            use iced::futures::future::{Either, select};
            let timer = Box::pin(bus.delay(Duration::from_secs(u64::from(request.delay))));
            let cancelled = Box::pin(cancel.changed());
            match select(timer, cancelled).await {
                Either::Left((done, _)) => done?,
                Either::Right(_) => return Err("cancelled".into()),
            }
        }
        if *cancel.borrow() {
            return Err("cancelled".into());
        }
        if let Some(target) = &own {
            hidden = true;
            let reply = bus
                .call(
                    &comp,
                    "comp.window.minimize",
                    json!(target),
                    Duration::from_secs(5),
                )
                .await?;
            if reply["minimized"].as_bool() != Some(true) {
                return Err("compositor did not confirm Cap minimisation".into());
            }
        }
        let path = directory.join(format!("cap-{}.png", uuid::Uuid::now_v7()));
        let mut args = json!({"path":path,"cursor":request.cursor});
        if let Some(target) = &request.window {
            args["window"] = json!(target)
        } else if let Some(output) = &request.output {
            args["output"] = json!(output)
        }
        if request.mode == Mode::Region {
            let selection = bus
                .call(
                    &comp,
                    "comp.region.select",
                    request
                        .output
                        .as_ref()
                        .map_or(json!({}), |o| json!({"output":o})),
                    Duration::from_secs(38),
                )
                .await?;
            if selection["status"] != "selected" {
                return Err(format!(
                    "selection {}",
                    selection["status"].as_str().unwrap_or("failed")
                ));
            }
            args["output"] = selection["output"].clone();
            args["region"] = selection["region"].clone();
            args["output_generation"] = selection["output_generation"].clone();
        }
        if *cancel.borrow() {
            return Err("cancelled".into());
        }
        let metadata = bus
            .call(&comp, "comp.capture.frame", args, Duration::from_secs(8))
            .await?;
        let document = Document::open(&path)?;
        Ok(Captured {
            document,
            path,
            metadata,
        })
    }
    .await;
    if hidden && let Some(target) = own {
        let restored = bus
            .call(
                &comp,
                "comp.window.restore",
                json!(target),
                Duration::from_secs(5),
            )
            .await;
        match restored {
            Ok(reply) if reply["minimized"].as_bool() == Some(false) => {}
            other => {
                return Err(format!(
                    "{}; Cap restoration failed: {other:?}",
                    result
                        .as_ref()
                        .err()
                        .map_or("capture finished", String::as_str)
                ));
            }
        }
    }
    result
}
pub fn absolute(path: &str) -> Result<PathBuf, String> {
    let p = Path::new(path);
    if p.is_absolute() {
        Ok(p.into())
    } else {
        std::env::current_dir()
            .map(|d| d.join(p))
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn lost_minimise_reply_still_restores_the_fenced_window() {
        let (bus, mut effects) = BusHandle::response_sink();
        let target = Target {
            id: 4,
            generation: 9,
        };
        let expected = target.clone();
        let driver = tokio::spawn(async move {
            let Some(crate::bus::Effect::Call {
                verb, args, reply, ..
            }) = effects.recv().await
            else {
                panic!("expected minimise")
            };
            assert_eq!(verb, "comp.window.minimize");
            assert_eq!(args, json!(expected));
            reply.send(Err("acknowledgement lost".into())).unwrap();
            let Some(crate::bus::Effect::Call {
                verb, args, reply, ..
            }) = effects.recv().await
            else {
                panic!("expected restore")
            };
            assert_eq!(verb, "comp.window.restore");
            assert_eq!(args, json!(expected));
            reply.send(Ok(json!({"minimized":false}))).unwrap();
        });
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let result = take(
            bus,
            "comp.test".into(),
            Request::default(),
            Some(target),
            PathBuf::from("/tmp"),
            rx,
        )
        .await;
        assert_eq!(result.unwrap_err(), "acknowledgement lost");
        driver.await.unwrap();
    }
    #[tokio::test]
    async fn cancellation_during_delay_never_hides_cap() {
        let (bus, mut effects) = BusHandle::response_sink();
        let (tx, rx) = tokio::sync::watch::channel(false);
        let driver = tokio::spawn(async move {
            let Some(crate::bus::Effect::Delay { duration, reply }) = effects.recv().await else {
                panic!("expected delay first")
            };
            assert_eq!(duration, Duration::from_secs(10));
            tx.send(true).unwrap();
            let _hold = reply;
            assert!(effects.recv().await.is_none());
        });
        let result = take(
            bus,
            "comp.test".into(),
            Request {
                delay: 10,
                ..Default::default()
            },
            Some(Target {
                id: 4,
                generation: 9,
            }),
            PathBuf::from("/tmp"),
            rx,
        )
        .await;
        assert_eq!(result.unwrap_err(), "cancelled");
        driver.await.unwrap();
    }
    #[test]
    fn invalid_requests_are_refused() {
        assert!(
            Request {
                delay: 11,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Request {
                mode: Mode::Window,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(serde_json::from_value::<Request>(json!({"mode":"screen","extra":true})).is_err());
    }
    #[test]
    fn window_identity_is_never_guessed() {
        assert!(windows(&json!({"windows":[{"id":1}]})).is_empty());
        let rows =
            windows(&json!({"windows":[{"id":3,"generation":9,"title":"Editor","focused":true}]}));
        assert_eq!(
            rows[0].target,
            Target {
                id: 3,
                generation: 9
            }
        );
    }
}
