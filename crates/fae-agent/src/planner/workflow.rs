#[async_trait::async_trait]
pub trait Node{
    fn name(&self) -> String;
    fn ty(&self) -> String;
    fn run(&self);
}

#[async_trait::async_trait]
pub trait Workflow{

}