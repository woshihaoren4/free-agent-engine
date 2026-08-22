mod plan_define;
mod plan_builder;

use std::any::{Any, TypeId};
pub use plan_define::*;
pub use plan_builder::*;


pub fn to_plan_ty<T: ?Sized + 'static>() -> String {
    format!("{:?}", TypeId::of::<T>())
}
