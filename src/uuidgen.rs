use uuid::Uuid;

pub fn gen_uuid() -> Uuid {
    Uuid::new_v4()
}
