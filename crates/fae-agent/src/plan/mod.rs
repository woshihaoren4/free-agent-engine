mod plan_builder;
mod plan_define;

pub use plan_builder::*;
pub use plan_define::*;
use std::any::{Any, TypeId};

pub fn to_plan_ty<T: ?Sized + 'static>() -> String {
    format!("{:?}", TypeId::of::<T>())
}
