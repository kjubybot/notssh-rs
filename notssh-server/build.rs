fn main() {
    tonic_build::configure()
        .out_dir("../gen")
        .compile(
            &["../proto/notssh.proto", "../proto/notsshcli.proto"],
            &[".."],
        )
        .unwrap()
}
