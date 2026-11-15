use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Default, Clone)]
pub(in crate::players) enum Omissible<T> {
    #[default]
    Omitted,
    Present(T),
}

impl<T> Omissible<T> {
    pub(in crate::players) fn is_present(&self) -> bool {
        matches!(self, Self::Present(_))
    }

    pub(in crate::players) fn is_omitted(&self) -> bool {
        matches!(self, Self::Omitted)
    }

    pub(in crate::players) fn into_option(self) -> Option<T> {
        match self {
            Self::Omitted => None,
            Self::Present(value) => Some(value),
        }
    }
}

impl<'de, T> Deserialize<'de> for Omissible<T>
where
    T: DeserializeOwned,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Self::Present)
    }
}

impl<T> Serialize for Omissible<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Omitted => serializer.serialize_unit(),
            Self::Present(value) => value.serialize(serializer),
        }
    }
}
