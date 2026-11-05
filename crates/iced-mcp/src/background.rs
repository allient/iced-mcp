//! Keeps an app the server launched for automation from taking keyboard
//! focus away from the developer (`ICED_MCP_BACKGROUND=1`).
//!
//! macOS is the only platform where an app can do this for itself: winit
//! 0.30 activates the app when it finishes launching
//! (`activateIgnoringOtherApps: YES`) and iced gives no way to change that.
//! So the bridge watches the app's own activation and hands it straight back
//! to the app that was frontmost, unless a mouse click caused it (the
//! developer chose the window). It also turns the app into an accessory app
//! (no Dock icon, not in Cmd-Tab, no menu bar) and opts it out of App Nap, so
//! a covered window keeps answering. Injected input does not need focus: it
//! is delivered inside the widget tree.
//!
//! Nothing here touches `NSApp` before winit creates it (winit insists on
//! its own `NSApplication` subclass): the observers only run once the app
//! is up.

#[cfg(target_os = "macos")]
pub(crate) fn install() -> bool {
    macos::install()
}

/// Other platforms: nothing the app can do for itself; see the guide.
#[cfg(not(target_os = "macos"))]
pub(crate) fn install() -> bool {
    false
}

#[cfg(target_os = "macos")]
mod macos {
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::NSObjectProtocol;
    use objc2::sel;
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy,
        NSApplicationDidBecomeActiveNotification, NSApplicationWillBecomeActiveNotification,
        NSEventType, NSRunningApplication, NSWorkspace,
    };
    use objc2_foundation::{
        MainThreadMarker, NSActivityOptions, NSNotification, NSNotificationCenter, NSProcessInfo,
        NSString,
    };

    use std::cell::RefCell;
    use std::ptr::NonNull;
    use std::rc::Rc;

    type Previous = Rc<RefCell<Option<Retained<NSRunningApplication>>>>;

    pub(super) fn install() -> bool {
        let Some(mtm) = MainThreadMarker::new() else {
            eprintln!("iced-mcp: background launch needs the main thread; the app may take focus");
            return false;
        };
        // SAFETY: plain AppKit/Foundation calls on the main thread; the
        // observers and the activity token live as long as the process.
        unsafe {
            let me = NSRunningApplication::currentApplication().processIdentifier();
            let previous: Previous = Rc::new(RefCell::new(frontmost_other(me)));

            let remember = previous.clone();
            let will = RcBlock::new(move |_: NonNull<NSNotification>| {
                // Still the app that is losing activation.
                if let Some(app) = frontmost_other(me) {
                    *remember.borrow_mut() = Some(app);
                }
            });
            let give_back = previous.clone();
            let did = RcBlock::new(move |_: NonNull<NSNotification>| {
                let app = NSApplication::sharedApplication(mtm);
                app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
                if clicked(&app) {
                    return;
                }
                match give_back.borrow().as_ref() {
                    Some(previous) => {
                        if app.respondsToSelector(sel!(yieldActivationToApplication:)) {
                            app.yieldActivationToApplication(previous);
                        }
                        previous.activateWithOptions(NSApplicationActivationOptions::empty());
                    }
                    None => app.deactivate(),
                }
            });

            let center = NSNotificationCenter::defaultCenter();
            let will = center.addObserverForName_object_queue_usingBlock(
                Some(NSApplicationWillBecomeActiveNotification),
                None,
                None,
                &will,
            );
            let did = center.addObserverForName_object_queue_usingBlock(
                Some(NSApplicationDidBecomeActiveNotification),
                None,
                None,
                &did,
            );
            std::mem::forget((will, did));

            // A hidden or covered app is otherwise napped: its frames, and
            // so every request to it, slow to a crawl.
            let activity = NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                NSActivityOptions::NSActivityUserInitiatedAllowingIdleSystemSleep,
                &NSString::from_str("driven by iced-mcp"),
            );
            std::mem::forget(activity);
        }
        true
    }

    /// The frontmost app, unless it is this one.
    unsafe fn frontmost_other(me: i32) -> Option<Retained<NSRunningApplication>> {
        NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .filter(|app| app.processIdentifier() != me)
    }

    /// Whether the event being handled is a mouse press: the developer
    /// clicked the window, so the activation is theirs to keep.
    fn clicked(app: &NSApplication) -> bool {
        app.currentEvent().is_some_and(|event| {
            matches!(
                // SAFETY: reading an event's type.
                unsafe { event.r#type() },
                NSEventType::LeftMouseDown
                    | NSEventType::RightMouseDown
                    | NSEventType::OtherMouseDown
            )
        })
    }
}
