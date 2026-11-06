# nmspace 群聊 @ 提及（mention）方案（终版：纯文本机制增强）

> 20261106 · javier · 已实现（M1+M2）

## 一、结论（与初版设计的差异）

初版设计曾提过「Gram.meta_json 线协议元数据」方案，**已否决回退**：评审时发现群里已有
一套「原始版 @」——工具栏插 `@名字` 纯文本 + Delegate 群助手按显示名匹配 @我（delegate.js
`mentionsMe`）。终版完全建在这个纯文本机制上，**协议/节点/Rust 零改动**，互通天然兼容。

## 二、功能清单（全部纯文本机制）

1. **输入联想**：输入 `@` 弹成员面板（过滤/↑↓/Enter/Tab/Esc）；工具栏「提及成员」按钮同链路
2. **气泡高亮**：接收端按当前群成员名单反查文本里的 `@名字` 片段（最长名优先 + 后边界校验，
   防「张三」吃「张三丰」）；@我 金色、@所有人 红色、其余蓝色；hover 下划线
3. **@N 徽标**：新消息命中「我的显示名集合」或 `@所有人` 令牌 → 会话列表金色 `@N`
   （UNREAD_AT：im_events 三处自增点 + 各清零点 + localStorage 持久化）
4. **@所有人权限**：>50 人群仅群主/管理员可发送（发送前文本检测，超限 toast 阻断）
5. **点 @ 跳资料卡**：气泡内 `.mention[data-id]` → showEntityDetail

## 三、@我 判定（im_events.isMentionTarget）

与 `Delegate.mentionsMe` 同款：取 Profile.displayName + Identity.nameOf 组成的显示名集合，
大小写不敏感匹配 `@名字`，词边界（后随空白/标点/串尾）防子串误判；另识别
`@(所有人|全部|everyone|all)` 令牌。改名/多显示名天然随资料走。

## 四、已知妥协（纯文本机制的代价）

- **同名成员**：反查按名字命中，同名会一起高亮、badge 无法区分到 id（可接受；精确方案需线协议
  元数据，如需可重启 meta_json 设计——proto 草案见 git 历史）
- **改名后历史消息**：高亮按当前名单匹配，旧名 @ 不再高亮（与 Delegate 行为一致）
- 手打一个「恰好是成员名」的 @（并非想 @）也会高亮/计徽标（主流 IM 同样如此）

## 五、改动文件（全部前端）

| 文件 | 内容 |
|---|---|
| composer.js | at 联想面板（atUpdate/atPanelKeys/atApply）、mentionHtml 名字反查高亮、bubble 挂钩、.mention 点击资料卡 |
| im_conversation.js | sendMsg @所有人权限（文本检测 + 阻断） |
| im_events.js | isMentionTarget/bumpUnreadAt（三处未读自增点挂 @ 计数） |
| im.js / im_lists.js | UNREAD_AT 声明/持久化/6 处清零点、itemHtml 金色 @N 徽标 |
| layout.css | .mention/.mention--me/.mention--all/.unread--at/.im-pop is-on 样式 |

**未动**：nm-proto / nm-client / nm-node / src-tauri（已全部回退至主干）、slimMsg 白名单、
Delegate 逻辑（mentionsMe 仍按名字匹配，与新功能互不干扰）。
