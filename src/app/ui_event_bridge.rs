use std::cell::RefCell;

thread_local! {
    static HOTKEY_HANDLER: RefCell<Option<Box<dyn FnMut(String)>>> = RefCell::new(None);
    static TOAST_HANDLER: RefCell<Option<Box<dyn FnMut(String)>>> = RefCell::new(None);
    static LOUDNESS_STATUS_REFRESH_HANDLER: RefCell<Option<Box<dyn FnMut()>>> =
        RefCell::new(None);
}

pub fn set_hotkey_handler(f: impl FnMut(String) + 'static) {
    HOTKEY_HANDLER.with(|handler| *handler.borrow_mut() = Some(Box::new(f)));
}

pub fn post_hotkey(id: String) {
    glib::MainContext::default().invoke(move || {
        HOTKEY_HANDLER.with(|handler| {
            if let Some(handler) = handler.borrow_mut().as_mut() {
                handler(id);
            }
        });
    });
}

pub fn set_toast_handler(f: impl FnMut(String) + 'static) {
    TOAST_HANDLER.with(|handler| *handler.borrow_mut() = Some(Box::new(f)));
}

pub fn post_toast(message: String) {
    glib::MainContext::default().invoke(move || {
        TOAST_HANDLER.with(|handler| {
            if let Some(handler) = handler.borrow_mut().as_mut() {
                handler(message);
            }
        });
    });
}

pub fn set_loudness_status_refresh_handler(f: impl FnMut() + 'static) {
    LOUDNESS_STATUS_REFRESH_HANDLER.with(|handler| *handler.borrow_mut() = Some(Box::new(f)));
}

pub fn post_loudness_status_refresh() {
    glib::MainContext::default().invoke(move || {
        LOUDNESS_STATUS_REFRESH_HANDLER.with(|handler| {
            if let Some(handler) = handler.borrow_mut().as_mut() {
                handler();
            }
        });
    });
}
