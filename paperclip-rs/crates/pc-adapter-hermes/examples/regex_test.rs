use pc_adapter_hermes::parse_output::parse_hermes_output;
fn main() {
    let stdout = "Session: session-abc123\nResponse: hello\n";
    let parsed = parse_hermes_output(stdout, "");
    println!("Parsed: {:?}", parsed);
}
