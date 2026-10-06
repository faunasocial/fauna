use fauna_provisioning::registrar::Registrar;

#[tokio::test]
async fn registrar_trait_has_check_register_methods() {
    fn _assert_trait<R: Registrar>(_: &R) {}
    // Compile-time assertion: the trait has the methods we expect.
}
