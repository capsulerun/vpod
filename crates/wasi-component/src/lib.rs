mod api;
mod repl;
mod vm;

wit_bindgen::generate!({
    world: "library",
    path: "vpod.wit",
});

use api::executor::Executor;
export!(Executor);
