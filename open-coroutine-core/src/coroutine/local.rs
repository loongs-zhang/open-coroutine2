use dashmap::DashMap;
use std::ffi::c_void;

/// A struct for coroutines handles local args.
#[derive(Debug, Default)]
pub struct CoroutineLocal<'c>(DashMap<&'c str, *mut c_void>);

#[allow(missing_docs, box_pointers)]
impl<'c> CoroutineLocal<'c> {
    pub fn put<V>(&self, key: &'c str, val: V) -> Option<V> {
        let v = Box::leak(Box::new(val));
        self.0
            .insert(key, (v as *mut V).cast::<c_void>())
            .map(|ptr| unsafe { *Box::from_raw(ptr.cast::<V>()) })
    }

    #[must_use]
    pub fn get<V>(&self, key: &'c str) -> Option<&V> {
        self.0.get(key).map(|ptr| unsafe { &*ptr.cast::<V>() })
    }

    #[must_use]
    pub fn get_mut<V>(&self, key: &'c str) -> Option<&mut V> {
        self.0.get(key).map(|ptr| unsafe { &mut *ptr.cast::<V>() })
    }

    #[must_use]
    pub fn remove<V>(&self, key: &'c str) -> Option<V> {
        self.0
            .remove(key)
            .map(|ptr| unsafe { *Box::from_raw(ptr.1.cast::<V>()) })
    }
}
