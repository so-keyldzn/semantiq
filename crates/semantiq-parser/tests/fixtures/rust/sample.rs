use std::collections::HashMap;

/// Doc
pub struct User {
    pub name: String,
}

pub enum Status {
    Active,
    Inactive,
}

pub trait Greetable {
    fn greet(&self) -> String;
}

impl Greetable for User {
    fn greet(&self) -> String {
        format!("Hello, {}!", self.name)
    }
}

pub fn process(items: &[User]) -> usize {
    items.len()
}

pub const MAX: usize = 100;

mod helpers {
    pub fn util() {}
}
