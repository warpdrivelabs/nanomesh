use super::*;

impl Session {
    /// 创建群（caller 为 owner 兼首成员）；返回 group_id。
    pub async fn group_create(
        &self,
        group_id: [u8; 32],
        name: &str,
    ) -> Result<[u8; 32], ClientError> {
        let op = GroupOp {
            group_id: group_id.to_vec(),
            name: name.to_string(),
            ..Default::default()
        };
        self.group_op("group.create", op).await?;
        Ok(group_id)
    }

    /// F2：创建「带密钥群」——group_id = 新生成的 Ed25519 公钥；私钥随 create 命令交给 home 节点
    /// （经加密的 client→home 信道），home 节点据此用 pkarr 发布「群公钥 → seed 节点」发现记录，
    /// 使成员节点无锚（不依赖 home_node 常驻）也能发现并 join 该群主题。返回 group_id（= 群公钥）。
    /// 传统 [`Self::group_create`]（调用方给定随机 id、仅火管发现）保持不变。
    pub async fn group_create_keyed(&self, name: &str) -> Result<[u8; 32], ClientError> {
        let sk = nm_transport::SecretKey::generate();
        let group_id = *sk.public().as_bytes();
        let op = GroupOp {
            group_id: group_id.to_vec(),
            name: name.to_string(),
            secret: sk.to_bytes().to_vec(),
            ..Default::default()
        };
        self.group_op("group.create", op).await?;
        Ok(group_id)
    }

    pub async fn group_join(&self, group_id: [u8; 32]) -> Result<(), ClientError> {
        self.group_op(
            "group.join",
            GroupOp {
                group_id: group_id.to_vec(),
                ..Default::default()
            },
        )
        .await
    }

    pub async fn group_leave(&self, group_id: [u8; 32]) -> Result<(), ClientError> {
        self.group_op(
            "group.leave",
            GroupOp {
                group_id: group_id.to_vec(),
                ..Default::default()
            },
        )
        .await
    }

    pub async fn group_kick(
        &self,
        group_id: [u8; 32],
        target: [u8; 32],
    ) -> Result<(), ClientError> {
        self.group_op(
            "group.kick",
            GroupOp {
                group_id: group_id.to_vec(),
                target: target.to_vec(),
                ..Default::default()
            },
        )
        .await
    }

    /// owner/admin 添加成员。
    pub async fn group_add(&self, group_id: [u8; 32], target: [u8; 32]) -> Result<(), ClientError> {
        self.group_op(
            "group.add",
            GroupOp {
                group_id: group_id.to_vec(),
                target: target.to_vec(),
                ..Default::default()
            },
        )
        .await
    }

    /// owner 提升成员为管理员。
    pub async fn group_promote(
        &self,
        group_id: [u8; 32],
        target: [u8; 32],
    ) -> Result<(), ClientError> {
        self.group_op(
            "group.promote",
            GroupOp {
                group_id: group_id.to_vec(),
                target: target.to_vec(),
                ..Default::default()
            },
        )
        .await
    }

    /// owner 取消某人的管理员。
    pub async fn group_demote(
        &self,
        group_id: [u8; 32],
        target: [u8; 32],
    ) -> Result<(), ClientError> {
        self.group_op(
            "group.demote",
            GroupOp {
                group_id: group_id.to_vec(),
                target: target.to_vec(),
                ..Default::default()
            },
        )
        .await
    }

    /// owner/admin 改群名。
    pub async fn group_rename(&self, group_id: [u8; 32], name: &str) -> Result<(), ClientError> {
        self.group_op(
            "group.rename",
            GroupOp {
                group_id: group_id.to_vec(),
                name: name.to_string(),
                ..Default::default()
            },
        )
        .await
    }

    /// owner/admin 设置群信息（名称/简介/头像）。
    pub async fn group_set_meta(
        &self,
        group_id: [u8; 32],
        name: &str,
        topic: &str,
        avatar_url: &str,
    ) -> Result<(), ClientError> {
        self.group_op(
            "group.set_meta",
            GroupOp {
                group_id: group_id.to_vec(),
                name: name.to_string(),
                topic: topic.to_string(),
                avatar_url: avatar_url.to_string(),
                ..Default::default()
            },
        )
        .await
    }

    /// owner 解散群。
    pub async fn group_dissolve(&self, group_id: [u8; 32]) -> Result<(), ClientError> {
        self.group_op(
            "group.dissolve",
            GroupOp {
                group_id: group_id.to_vec(),
                ..Default::default()
            },
        )
        .await
    }

    pub async fn group_list(&self) -> Result<Vec<Group>, ClientError> {
        let res = rpc_over(&self.conn, self.my_id, "group.list", None).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let payload = res
            .result
            .ok_or_else(|| ClientError::Other("no payload".into()))?;
        Ok(GroupList::decode(payload.value.as_slice())
            .map_err(err)?
            .groups)
    }

    async fn group_op(&self, method: &str, op: GroupOp) -> Result<(), ClientError> {
        let params = Any {
            type_url: "nmspace.v1.GroupOp".to_string(),
            value: op.encode_to_vec(),
        };
        let res = rpc_over(&self.conn, self.my_id, method, Some(params)).await?;
        if res.ok {
            Ok(())
        } else {
            Err(ClientError::Other(res.error))
        }
    }
}
