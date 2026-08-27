// Copyright 2026 The AccessKit Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0 (found in
// the LICENSE-APACHE file).

use accesskit::{ActionHandler, ActivationHandler, DeactivationHandler, TreeUpdate};
use winit::{
    event::WindowEvent, event_loop::ActiveEventLoop, platform::web::WindowExtWebSys, window::Window,
};

pub struct Adapter {
    inner: accesskit_web::Adapter,
}

impl Adapter {
    pub fn new(
        _event_loop: &ActiveEventLoop,
        window: &Window,
        activation_handler: impl 'static + ActivationHandler,
        action_handler: impl 'static + ActionHandler,
        deactivation_handler: impl 'static + DeactivationHandler,
    ) -> Self {
        let canvas = window
            .canvas()
            .expect("the winit web window must have a canvas");
        let inner = accesskit_web::Adapter::new(
            canvas,
            activation_handler,
            action_handler,
            deactivation_handler,
        )
        .expect("failed to initialize the AccessKit web adapter");
        Self { inner }
    }

    pub fn update_if_active(&mut self, updater: impl FnOnce() -> TreeUpdate) {
        self.inner.update_if_active(updater);
    }

    pub fn process_event(&mut self, _window: &Window, _event: &WindowEvent) {}
}
