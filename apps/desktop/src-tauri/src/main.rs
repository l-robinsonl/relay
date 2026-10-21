//! Entry point. All of the work happens in the library so that the same code
//! can be linked on desktop and, later, mobile targets.

fn main() {
    relay_desktop_lib::run()
}
