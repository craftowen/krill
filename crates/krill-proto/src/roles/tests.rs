use super::*;
use crate::ipc;

#[test]
fn fixed_wire_roundtrips() {
    for (request, fixture) in [
        (
            Request::Set(Role::Interactive),
            vec![3, 0, 0, 0, 0x17, 0, 0],
        ),
        (Request::Set(Role::Observer), vec![3, 0, 0, 0, 0x17, 0, 1]),
        (
            Request::Resize { cols: 80, rows: 24 },
            vec![4, 0, 0, 0, 0x17, 1, 80, 24],
        ),
        (Request::Focus, vec![2, 0, 0, 0, 0x17, 2]),
    ] {
        assert_eq!(request.encode().unwrap(), fixture);
        let (k, b) = ipc::read_frame(&mut fixture.as_slice()).unwrap().unwrap();
        assert_eq!(Request::decode(k, &b).unwrap(), request);
    }
    for (ack, byte) in [
        (Ack::Applied, 0),
        (Ack::Denied, 1),
        (Ack::Invalid, 2),
        (Ack::Closed, 3),
    ] {
        let fixture = vec![2, 0, 0, 0, 0x2c, byte];
        assert_eq!(ack.encode().unwrap(), fixture);
        let (k, b) = ipc::read_frame(&mut fixture.as_slice()).unwrap().unwrap();
        assert_eq!(Ack::decode(k, &b).unwrap(), ack);
    }
}

proptest::proptest! {
    #[test]
    fn arbitrary_roles_never_panic(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..128)) {
        Request::decode(kind::ROLE, &bytes).ok();
        Ack::decode(kind::ROLE_ACK, &bytes).ok();
    }

    #[test]
    fn resize_roundtrips(cols: u16, rows: u16) {
        let req = Request::Resize { cols, rows };
        let frame = req.encode().unwrap();
        let (k, b) = ipc::read_frame(&mut frame.as_slice()).unwrap().unwrap();
        proptest::prop_assert_eq!(Request::decode(k, &b).unwrap(), req);
    }
}

#[test]
fn non_size_owner_is_append_only() {
    let req = Request::Set(Role::InteractiveWithSize { owns_size: false });
    let fixture = vec![4, 0, 0, 0, 0x17, 0, 2, 0];
    assert_eq!(req.encode().unwrap(), fixture);
    let (k, b) = ipc::read_frame(&mut fixture.as_slice()).unwrap().unwrap();
    assert_eq!(Request::decode(k, &b).unwrap(), req);
}
