//! The host's Bus verbs.

use crate::panels::PanelVerb;

/// One `shell.scene.*` request (Scene Editor plan §4.3; Quoin's verb set),
/// plus the `shell.scenes.list` inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SceneVerb {
    Load,
    Validate,
    Patch,
    Get,
    Describe,
    Unload,
    Watch,
    /// Measured from the renderer, not the store.
    Layout,
    List,
    /// Map / unmap the dialog seat (Scene Editor plan §4.3 Q2).
    DialogShow,
    DialogHide,
    /// `shell.ping`: `{service, status: "ok"}`, as Quoin answers it (the
    /// liveness probe loaders and gates wait on).
    Ping,
    /// `shell.info`: the contract and the verbs this host serves.
    Info,
    /// `shell.props.get`: `{dialog, panels}` or one path of it.
    PropsGet,
    /// `shell.panel.state {edge}` (Quoin's panel model).
    PanelState,
    /// `shell.panel.mode {edge, mode}`.
    PanelMode,
    /// `shell.panel.page.set {edge, id}`.
    PanelPageSet,
    /// `shell.panel.order {edges}`: conf.mix's declared page order (N2b).
    PanelOrder,
    /// `shell.panel.resize {edge, thickness_px}` (N2b).
    PanelResize,
    /// Agent equivalents of the RMB menu: open {corner}, choose {serial,index}, close {serial}.
    MenuOpen,
    MenuChoose,
    MenuClose,
    /// `shell.panel.{show,hide,toggle,pin,unpin,dock} {edge}`, or with
    /// `corner` set `shell.corner.{show,hide,toggle,pin,unpin} {corner}` on
    /// the edge the corner summons (Quoin: top-left → left, …).
    Panel { verb: crate::panels::PanelVerb, corner: bool },
    /// A Quoin shell verb this host does not serve yet (the settings verbs):
    /// answered `not_served`, never a
    /// silent no-op, and never listed by `shell.info`.
    NotServed,
}

impl SceneVerb {
    /// The verb for a request's `command`: `shell.<verb>` (the contract's
    /// spelling, which loaders send whatever the host registered as), or
    /// `<service>.<verb>` under an override name.
    pub fn parse(service: &str, command: &str) -> Option<Self> {
        fn under<'a>(command: &'a str, prefix: &str) -> Option<&'a str> {
            command.strip_prefix(prefix)?.strip_prefix('.')
        }
        let verb = under(command, "shell").or_else(|| under(command, service))?;
        Some(match verb {
            "scene.load" => Self::Load,
            "scene.validate" => Self::Validate,
            "scene.patch" => Self::Patch,
            "scene.get" => Self::Get,
            "scene.describe" => Self::Describe,
            "scene.unload" => Self::Unload,
            "scene.watch" => Self::Watch,
            "scene.layout" => Self::Layout,
            "scenes.list" => Self::List,
            "dialog.show" => Self::DialogShow,
            "dialog.hide" => Self::DialogHide,
            "ping" => Self::Ping,
            "info" => Self::Info,
            "props.get" => Self::PropsGet,
            "panel.state" => Self::PanelState,
            "panel.mode" => Self::PanelMode,
            "panel.page.set" => Self::PanelPageSet,
            "panel.order" => Self::PanelOrder,
            "panel.resize" => Self::PanelResize,
            "corner.menu.open" => Self::MenuOpen,
            "corner.menu.choose" => Self::MenuChoose,
            "corner.menu.close" => Self::MenuClose,
            "panel.show" => Self::Panel { verb: PanelVerb::Show, corner: false },
            "panel.hide" => Self::Panel { verb: PanelVerb::Hide, corner: false },
            "panel.toggle" => Self::Panel { verb: PanelVerb::Toggle, corner: false },
            "panel.pin" => Self::Panel { verb: PanelVerb::Pin, corner: false },
            "panel.unpin" => Self::Panel { verb: PanelVerb::Unpin, corner: false },
            "panel.dock" => Self::Panel { verb: PanelVerb::Dock, corner: false },
            "corner.show" => Self::Panel { verb: PanelVerb::Show, corner: true },
            "corner.hide" => Self::Panel { verb: PanelVerb::Hide, corner: true },
            "corner.toggle" => Self::Panel { verb: PanelVerb::Toggle, corner: true },
            "corner.pin" => Self::Panel { verb: PanelVerb::Pin, corner: true },
            "corner.unpin" => Self::Panel { verb: PanelVerb::Unpin, corner: true },
            settings if settings.starts_with("settings.") => Self::NotServed,
            _ => return None,
        })
    }

    /// Verbs that can change what is mounted or drawn.
    pub fn mutates(self) -> bool {
        matches!(
            self,
            Self::Load
                | Self::Patch
                | Self::Unload
                | Self::DialogShow
                | Self::DialogHide
                | Self::PanelMode
                | Self::PanelPageSet
                | Self::PanelOrder
                | Self::PanelResize
                | Self::MenuOpen
                | Self::MenuChoose
                | Self::MenuClose
                | Self::Panel { .. }
        )
    }

    /// Every verb name under `service`, for HELP and the tests.
    pub fn names(service: &str) -> Vec<String> {
        [
            "scene.load", "scene.validate", "scene.patch", "scene.get", "scene.describe",
            "scene.unload", "scene.watch", "scene.layout", "scenes.list", "dialog.show", "dialog.hide",
            "ping", "info", "props.get", "panel.state", "panel.mode", "panel.page.set",
            "panel.order", "panel.resize", "panel.show", "panel.hide", "panel.toggle", "panel.pin",
            "panel.unpin", "panel.dock", "corner.show", "corner.hide", "corner.toggle", "corner.pin",
            "corner.unpin",
            "corner.menu.open", "corner.menu.choose", "corner.menu.close",
        ]
        .into_iter()
        .map(|verb| format!("{service}.{verb}"))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbs_parse_as_shell_or_under_the_registered_service() {
        for name in SceneVerb::names("shell") {
            assert!(SceneVerb::parse("shell", &name).is_some(), "{name}");
            assert!(SceneVerb::parse("compd-scenes", &name).is_some(), "{name}");
        }
        for name in SceneVerb::names("compd-scenes") {
            assert!(SceneVerb::parse("compd-scenes", &name).is_some(), "{name}");
            assert!(SceneVerb::parse("shell", &name).is_none(), "{name}");
        }
        assert_eq!(SceneVerb::parse("shell", "shellx.scene.load"), None);
        assert_eq!(SceneVerb::parse("shell", "shell.panel.nope"), None);
        for verb in ["shell.settings.get", "shell.settings.scheme"] {
            assert_eq!(SceneVerb::parse("shell", verb), Some(SceneVerb::NotServed), "{verb}");
        }
        assert_eq!(SceneVerb::parse("shell", "shell.panel.order"), Some(SceneVerb::PanelOrder));
        assert_eq!(
            SceneVerb::parse("shell", "shell.corner.show"),
            Some(SceneVerb::Panel { verb: PanelVerb::Show, corner: true })
        );
        assert_eq!(
            SceneVerb::parse("shell", "shell.panel.unpin"),
            Some(SceneVerb::Panel { verb: PanelVerb::Unpin, corner: false })
        );
        assert!(!SceneVerb::names("shell").iter().any(|name| name.contains("settings")), "shell.info lists only what is served");
    }
}
