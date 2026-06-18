use crate::MoveType;
use std::marker::PhantomData;
use sui_transaction_builder::Argument;

pub struct Arg<T>(Argument, PhantomData<T>);
pub struct Ref<'a, T>(Argument, PhantomData<&'a T>);
pub struct MutRef<'a, T>(Argument, PhantomData<&'a mut T>);

impl<T: MoveType> From<Argument> for Arg<T> {
    fn from(value: Argument) -> Self {
        Self(value, PhantomData)
    }
}

impl<T: MoveType> From<Argument> for Ref<'_, T> {
    fn from(value: Argument) -> Self {
        Self(value, PhantomData)
    }
}

impl<T: MoveType> From<Argument> for MutRef<'_, T> {
    fn from(value: Argument) -> Self {
        Self(value, PhantomData)
    }
}

impl<T> Arg<T> {
    pub fn borrow(&self) -> Ref<'_, T> {
        Ref(self.0.clone(), PhantomData)
    }

    pub fn borrow_mut(&mut self) -> MutRef<'_, T> {
        MutRef(self.0.clone(), PhantomData)
    }
}

impl<T> From<Arg<T>> for Argument {
    fn from(value: Arg<T>) -> Self {
        value.0
    }
}

impl<T> From<Ref<'_, T>> for Argument {
    fn from(value: Ref<'_, T>) -> Self {
        value.0
    }
}

impl<T> From<MutRef<'_, T>> for Argument {
    fn from(value: MutRef<'_, T>) -> Self {
        value.0
    }
}
