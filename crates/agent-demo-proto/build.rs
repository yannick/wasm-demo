//! Generates the buffa message types and connectrpc service stubs for `proto/agentdemo/v1`.
//! Needs `protoc` on PATH (or `PROTOC`).

fn main() {
    println!("cargo::rerun-if-changed=../../proto/agentdemo/v1/agent.proto");
    connectrpc_build::Config::new()
        .files(&["../../proto/agentdemo/v1/agent.proto"])
        .includes(&["../../proto"])
        .include_file("_connectrpc.rs")
        .compile()
        .expect("generating agentdemo.v1 failed");
}
