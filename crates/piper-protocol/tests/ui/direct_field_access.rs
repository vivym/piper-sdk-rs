fn main() {
    let frame = piper_protocol::frame::PiperFrame::new_standard(0x123, [1]).unwrap();
    let _ = frame.id;
}
