fn main() {
    prost_build::compile_protos(&["src/inferencer.proto"],
                                &["src/"]).unwrap();
}
