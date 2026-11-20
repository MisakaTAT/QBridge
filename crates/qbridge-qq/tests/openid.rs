use qbridge_qq::openid::{GroupOpenId, MemberOpenId, OpenIdError, UserOpenId};

#[test]
fn accepts_qofficials_openid_charset() {
    assert_eq!(
        UserOpenId::new("7A3B9C1D5E2F4A6B8C0D1E3F5A7B9C2D")
            .unwrap()
            .as_str(),
        "7A3B9C1D5E2F4A6B8C0D1E3F5A7B9C2D"
    );
    assert!(GroupOpenId::new("group_openid-1").is_ok());
    assert!(MemberOpenId::new("member_openid_1").is_ok());
}

#[test]
fn rejects_empty_and_unsafe_openids() {
    for value in ["", "with/slash", "with space", "with%20escape", "带中文"] {
        assert!(
            UserOpenId::new(value).is_err(),
            "{value:?} should be rejected"
        );
    }
}

#[test]
fn openid_error_is_reported() {
    let error: OpenIdError = GroupOpenId::new("").unwrap_err();
    assert_eq!(error.to_string(), "invalid openid");
}
