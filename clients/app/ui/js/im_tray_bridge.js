// ── 托盘 / 通知用到的会话入口 ──
window.imSnapshot = () => {
  const convos = {};
  const index = window.ChatLog ? (ChatLog.index().convos || {}) : {};
  for (const id of Object.keys(index)) if (index[id] && index[id].last) convos[id] = [index[id].last];
  return { myId: MY_ID, unread: UNREAD, convos, active: viewingPeer() };
};
window.imOpen = function (id) {
  if (!id || !MY_ID) return;
  if (window.Groups && Groups.byId(id)) openGroupChat(id);
  else if (window.Channels && Channels.byId(id)) openChannel(id);
  else selectContact(id);
};
window.imMarkRead = function (id) {
  if (!UNREAD[id]) return;
  UNREAD[id] = 0;
  scheduleUnread();
  renderPanels();
};
window.imMarkAllRead = function () {
  for (const id of Object.keys(UNREAD)) UNREAD[id] = 0;
  scheduleUnread();
  renderPanels();
};
window.imFlush = () => { if (window.ChatLog) ChatLog.setUnread(UNREAD); };
