// 界面文案。第一批只有简体中文和英文，缺词回退中文。用户起的名字和消息正文不在这里。
(function () {
  const PACKS = {
    "zh-CN": {
      "lang.zh": "中文",
      "lang.en": "English",
      "menu.lang": "语言",
      "menu.langSub": "界面显示语言",
      "day.today": "今天",
      "day.yesterday": "昨天",
      "conv.empty": "暂无消息",
      "conv.emptySub": "在下方输入，按 Enter 发送",
      "list.noGroup": "还没有群组。点上方加号新建。",
      "list.noGroupHit": "没有匹配的群",
      "list.noChannel": "还没有频道。点上方加号新建，或用订阅按钮粘贴频道 id。",
      "list.noChannelHit": "没有匹配的频道",
      "list.noChat": "还没有会话。去左侧「实体目录」选一个实体开始聊。",
      "panel.im": "消息",
      "panel.entities": "实体目录",
      "panel.nodes": "节点服务",
      "panel.groups": "群组",
      "panel.channels": "频道",
      "search.im": "搜索会话",
      "search.entities": "搜索实体",
      "search.nodes": "搜索节点服务",
      "search.groups": "搜索群组",
      "search.channels": "搜索频道",
      "conv.me": "我",
      "conv.people": "{n} 人",
      "conv.channel": "频道",
      "conv.search": "搜索此会话",
      "conv.members": "成员",
      "conv.profile": "查看资料",
      "conv.editChannel": "编辑频道信息",
      "conv.copyId": "复制 id",
      "conv.placeholder": "Enter 发送，Shift+Enter 换行",
      "conv.emoji": "表情",
      "conv.shot": "截图",
      "conv.shotHow": "截图方式",
      "conv.window": "截取窗口",
      "conv.pickWindow": "选择窗口",
      "conv.image": "图片",
      "conv.video": "视频",
      "conv.file": "文件",
      "conv.voice": "语音",
      "conv.mention": "提及成员",
      "conv.card": "发送名片",
      "conv.history": "会话记录",
      "conv.historyPh": "搜索当前会话",
      "conv.send": "发送",
      "conv.noHistory": "没有匹配的记录",
      "media.image": "[图片]",
      "media.voice": "[语音]",
      "media.video": "[视频]",
      "media.file": "[文件]",
      "media.card": "[名片]",
      "media.msg": "[消息]",
      "media.download": "点击下载",
      "media.cardName": "名片",
      "media.unsupported": "不支持的消息",
      "media.noCard": "没有可发送的名片",
      "role.owner": "群主",
      "role.admin": "管理员",
      "role.member": "成员",
      "grp.add": "加人",
      "grp.info": "群信息",
      "grp.dissolve": "解散",
      "grp.leave": "退群",
      "grp.members": "成员",
      "presence.online": "在线",
      "presence.away": "离开",
      "presence.busy": "忙碌",
      "presence.dnd": "勿扰",
      "presence.offline": "离线",
    },
    en: {
      "lang.zh": "中文",
      "lang.en": "English",
      "menu.lang": "Language",
      "menu.langSub": "Interface language",
      "day.today": "Today",
      "day.yesterday": "Yesterday",
      "conv.empty": "No messages yet",
      "conv.emptySub": "Type below, press Enter to send",
      "list.noGroup": "No groups yet. Use + to create one.",
      "list.noGroupHit": "No matching groups",
      "list.noChannel": "No channels yet. Use + to create one, or subscribe with an id.",
      "list.noChannelHit": "No matching channels",
      "list.noChat": "No conversations yet. Pick someone in Entities.",
      "panel.im": "Messages",
      "panel.entities": "Entities",
      "panel.nodes": "Node services",
      "panel.groups": "Groups",
      "panel.channels": "Channels",
      "search.im": "Search conversations",
      "search.entities": "Search entities",
      "search.nodes": "Search node services",
      "search.groups": "Search groups",
      "search.channels": "Search channels",
      "conv.me": "Me",
      "conv.people": "{n} people",
      "conv.channel": "Channel",
      "conv.search": "Search this chat",
      "conv.members": "Members",
      "conv.profile": "Profile",
      "conv.editChannel": "Edit channel",
      "conv.copyId": "Copy id",
      "conv.placeholder": "Enter to send, Shift+Enter for a new line",
      "conv.emoji": "Emoji",
      "conv.shot": "Screenshot",
      "conv.shotHow": "Screenshot options",
      "conv.window": "Capture window",
      "conv.pickWindow": "Choose window",
      "conv.image": "Image",
      "conv.video": "Video",
      "conv.file": "File",
      "conv.voice": "Voice",
      "conv.mention": "Mention",
      "conv.card": "Send a card",
      "conv.history": "History",
      "conv.historyPh": "Search this chat",
      "conv.send": "Send",
      "conv.noHistory": "No matching messages",
      "media.image": "[Image]",
      "media.voice": "[Voice]",
      "media.video": "[Video]",
      "media.file": "[File]",
      "media.card": "[Card]",
      "media.msg": "[Message]",
      "media.download": "Download",
      "media.cardName": "Card",
      "media.unsupported": "Unsupported message",
      "media.noCard": "No card to send",
      "role.owner": "Owner",
      "role.admin": "Admin",
      "role.member": "Member",
      "grp.add": "Add",
      "grp.info": "Info",
      "grp.dissolve": "Dissolve",
      "grp.leave": "Leave",
      "grp.members": "Members",
      "presence.online": "Online",
      "presence.away": "Away",
      "presence.busy": "Busy",
      "presence.dnd": "Do not disturb",
      "presence.offline": "Offline",
    },
  };

  function current() {
    try { return localStorage.getItem("nm-locale") || ""; } catch (_) { return ""; }
  }
  function resolve() {
    const saved = current();
    if (PACKS[saved]) return saved;
    const nav = (navigator.language || "zh-CN").toLowerCase();
    if (nav.startsWith("zh")) return "zh-CN";
    if (nav.startsWith("en")) return "en";
    return "zh-CN";
  }
  let LOCALE = "zh-CN";
  function t(key, vars) {
    const pack = PACKS[LOCALE] || PACKS["zh-CN"];
    let s = (pack && pack[key]) || PACKS["zh-CN"][key] || key;
    if (vars) Object.keys(vars).forEach((k) => { s = s.split("{" + k + "}").join(String(vars[k])); });
    return s;
  }
  function apply(root) {
    (root || document).querySelectorAll("[data-i18n]").forEach((el) => { el.textContent = t(el.dataset.i18n); });
    (root || document).querySelectorAll("[data-i18n-ph]").forEach((el) => { el.setAttribute("placeholder", t(el.dataset.i18nPh)); });
  }
  function setLocale(next) {
    if (!PACKS[next]) return;
    LOCALE = next;
    try { localStorage.setItem("nm-locale", next); } catch (_) {}
    apply(document);
    document.documentElement.lang = next === "en" ? "en" : "zh-CN";
    if (typeof renderConversations === "function") renderConversations();
    if (typeof window.renderGroupsList === "function") window.renderGroupsList();
    if (typeof window.renderChannelsList === "function") window.renderChannelsList();
    if (typeof renderConversation === "function") renderConversation();
  }
  LOCALE = resolve();
  document.documentElement.lang = LOCALE === "en" ? "en" : "zh-CN";
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", () => apply(document));
  else apply(document);
  window.t = t;
  window.nmLocale = () => LOCALE;
  window.nmSetLocale = setLocale;
})();
