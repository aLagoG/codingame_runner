// See ../baseline_cpp/src/main.rs for the rationale.

#[link(name = "tron_v1_cpp_inner", kind = "static")]
unsafe extern "C" {
    fn cgio_main() -> i32;
}

fn main() -> std::process::ExitCode {
    let code = unsafe { cgio_main() };
    std::process::ExitCode::from(code as u8)
}
