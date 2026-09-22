//! `im-entity` — 可扩展实体类型体系（见 `docs/PLAN_C_extensible_entities.md`）。
//!
//! 统一约定：身份=公钥 · 类型=命名空间字符串 · 载荷=`Any` · 能力(Capability)驱动路由。
//! 加新类型 = 实现 [`EntityKind`] 并 `register`，**核心协议/传输/身份层不变**。

use std::collections::BTreeMap;

use im_proto::pb::Capability;
use prost::Message;

#[derive(Debug, thiserror::Error)]
pub enum KindError {
    #[error("unknown kind: {0}")]
    Unknown(String),
    #[error("invalid profile: {0}")]
    InvalidProfile(String),
}

/// 运行时可存储的实体类型描述符（注册表条目）。
#[derive(Clone, Debug)]
pub struct KindDescriptor {
    pub kind: &'static str,
    pub capabilities: &'static [Capability],
    pub summary: &'static str,
}

/// 编译期实体类型：携带结构化 `Profile` 与能力集。
pub trait EntityKind {
    /// 命名空间类型名，如 `"agent.assistant"`。
    const KIND: &'static str;
    /// 该类型的结构化档案（protobuf 消息）。
    type Profile: Message + Default;

    fn capabilities() -> &'static [Capability];
    fn summary() -> &'static str;

    /// 校验档案（默认放行；具体类型可覆盖）。
    fn validate(_profile: &Self::Profile) -> Result<(), KindError> {
        Ok(())
    }

    fn descriptor() -> KindDescriptor {
        KindDescriptor {
            kind: Self::KIND,
            capabilities: Self::capabilities(),
            summary: Self::summary(),
        }
    }
}

/// 把某类型的 `Profile` 打包为线协议 `Any`（type_url = `"imspace.v1/<kind>"`）。
pub fn pack_profile<K: EntityKind>(profile: &K::Profile) -> im_proto::pb::Any {
    im_proto::pb::Any {
        type_url: format!("imspace.v1/{}", K::KIND),
        value: profile.encode_to_vec(),
    }
}

/// 从 `Any` 解出某类型的 `Profile`（校验 type_url 匹配）。
pub fn unpack_profile<K: EntityKind>(any: &im_proto::pb::Any) -> Result<K::Profile, KindError> {
    let want = format!("imspace.v1/{}", K::KIND);
    if any.type_url != want {
        return Err(KindError::InvalidProfile(format!(
            "type_url mismatch: want {want}, got {}",
            any.type_url
        )));
    }
    let profile = K::Profile::decode(any.value.as_slice())
        .map_err(|e| KindError::InvalidProfile(e.to_string()))?;
    K::validate(&profile)?;
    Ok(profile)
}

/// 类型注册表：`kind` 字符串 → 描述符。支持点分层级的前缀查询。
#[derive(Default)]
pub struct KindRegistry {
    kinds: BTreeMap<&'static str, KindDescriptor>,
}

impl KindRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<K: EntityKind>(&mut self) {
        let d = K::descriptor();
        self.kinds.insert(d.kind, d);
    }

    pub fn register_descriptor(&mut self, d: KindDescriptor) {
        self.kinds.insert(d.kind, d);
    }

    pub fn get(&self, kind: &str) -> Option<&KindDescriptor> {
        self.kinds.get(kind)
    }

    /// 前缀匹配，如 `"compute."` 命中训练 + 推理。
    pub fn resolve_prefix<'a>(
        &'a self,
        prefix: &'a str,
    ) -> impl Iterator<Item = &'a KindDescriptor> + 'a {
        self.kinds.values().filter(move |d| d.kind.starts_with(prefix))
    }

    pub fn all(&self) -> impl Iterator<Item = &KindDescriptor> {
        self.kinds.values()
    }

    /// 预置内置类型：人 / AI Agent / 推理算力 / 物联网设备。
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register::<kinds::Person>();
        r.register::<kinds::AgentAssistant>();
        r.register::<kinds::InferenceService>();
        r.register::<kinds::IotDevice>();
        r
    }
}

/// 内置实体类型。自定义类型照此实现 [`EntityKind`] 即可，无需改动核心。
pub mod kinds {
    use super::{EntityKind, KindError};
    use im_proto::pb::{AgentProfile, Capability, DeviceProfile, InferenceProfile, PersonProfile};

    /// 人类用户。
    pub struct Person;
    impl EntityKind for Person {
        const KIND: &'static str = "person";
        type Profile = PersonProfile;
        fn capabilities() -> &'static [Capability] {
            &[Capability::Message, Capability::Presence]
        }
        fn summary() -> &'static str {
            "人类用户"
        }
    }

    /// AI 助手智能体：多能力叠加（对话 + 工具调用 + 任务 + 流）。
    pub struct AgentAssistant;
    impl EntityKind for AgentAssistant {
        const KIND: &'static str = "agent.assistant";
        type Profile = AgentProfile;
        fn capabilities() -> &'static [Capability] {
            &[
                Capability::Message,
                Capability::Command,
                Capability::Job,
                Capability::Stream,
            ]
        }
        fn summary() -> &'static str {
            "AI 助手智能体"
        }
        fn validate(profile: &AgentProfile) -> Result<(), KindError> {
            if profile.backend.is_empty() {
                return Err(KindError::InvalidProfile("agent backend 不能为空".into()));
            }
            Ok(())
        }
    }

    /// 推理算力服务。
    pub struct InferenceService;
    impl EntityKind for InferenceService {
        const KIND: &'static str = "compute.inference";
        type Profile = InferenceProfile;
        fn capabilities() -> &'static [Capability] {
            &[Capability::Job, Capability::Stream, Capability::Command]
        }
        fn summary() -> &'static str {
            "推理算力服务"
        }
    }

    /// 物联网传感器设备。
    pub struct IotDevice;
    impl EntityKind for IotDevice {
        const KIND: &'static str = "device.sensor";
        type Profile = DeviceProfile;
        fn capabilities() -> &'static [Capability] {
            &[
                Capability::TelemetryPub,
                Capability::Command,
                Capability::Presence,
            ]
        }
        fn summary() -> &'static str {
            "物联网传感器设备"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use im_proto::pb::AgentProfile;

    #[test]
    fn registry_builtins() {
        let r = KindRegistry::with_builtins();
        assert!(r.get("agent.assistant").is_some());
        assert_eq!(r.get("person").unwrap().summary, "人类用户");
        let compute: Vec<_> = r.resolve_prefix("compute.").map(|d| d.kind).collect();
        assert!(compute.contains(&"compute.inference"));
        // agent 具备多能力
        let caps = r.get("agent.assistant").unwrap().capabilities;
        assert!(caps.contains(&Capability::Command) && caps.contains(&Capability::Job));
    }

    #[test]
    fn pack_unpack_roundtrip() {
        let p = AgentProfile {
            backend: "claude".into(),
            tools: vec!["search".into(), "code".into()],
            ..Default::default()
        };
        let any = pack_profile::<kinds::AgentAssistant>(&p);
        assert_eq!(any.type_url, "imspace.v1/agent.assistant");
        let back = unpack_profile::<kinds::AgentAssistant>(&any).unwrap();
        assert_eq!(back.backend, "claude");
        assert_eq!(back.tools.len(), 2);
    }

    #[test]
    fn validate_rejects_empty_backend() {
        let any = pack_profile::<kinds::AgentAssistant>(&AgentProfile::default());
        assert!(unpack_profile::<kinds::AgentAssistant>(&any).is_err());
    }
}
