use crate::Error;


#[async_trait::async_trait]
pub trait Env: Sync{
    async fn get<T>(&self, key: &str) -> Option<T>;
    async fn set<T>(&self, key: &str, value: T) -> Result<(), Error>;
}

#[async_trait::async_trait]
pub trait Channel:Sync{
    async fn send(&self)-> Result<(), Error>;
    async fn receive(&self)-> Result<(), Error>;
}


#[async_trait::async_trait]
pub trait Agent{
    async fn on_env<E:Env>(&mut self,env:E)-> Result<(),Error>;

    async fn call(&mut self)-> Result<(),Error>;
}

#[cfg(test)]
mod tests {

}