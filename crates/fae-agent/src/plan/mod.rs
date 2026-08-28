mod plan_builder;
mod plan_define;
mod single_agent;

pub use plan_builder::*;
pub use plan_define::*;
pub use single_agent::*;
use std::any::TypeId;

pub fn to_plan_ty<T: ?Sized + 'static>() -> String {
    format!("{:?}", TypeId::of::<T>())
}
