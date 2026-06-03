mod api;
mod repl;
mod vm;

wit_bindgen::generate!({
    world: "library",
    path: "capsulev.wit",
});

use api::executor::Executor;
export!(Executor);
