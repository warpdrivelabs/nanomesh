// 描边图标。1.5px、currentColor，活动栏 20 / 其余 16 由 CSS 控制尺寸。
const NM_ICON_PATHS = {
  chat: '<path d="M7.9 20A9 9 0 1 0 4 16.1L2 22Z"/>',
  contacts: '<rect x="3" y="5" width="18" height="14" rx="2"/><circle cx="9" cy="12" r="2"/><path d="M15 10h4M15 14h3"/>',
  nodes: '<rect x="3" y="4" width="18" height="6" rx="1.5"/><rect x="3" y="14" width="18" height="6" rx="1.5"/><path d="M7 7h.01M7 17h.01"/>',
  groups: '<circle cx="9" cy="8" r="3"/><circle cx="17" cy="9" r="2.2"/><path d="M3.5 19c.7-3 2.8-4.5 5.5-4.5s4.8 1.5 5.5 4.5M15.5 14.6c1.6.3 2.9 1.2 3.5 2.9"/>',
  channels: '<path d="M5 16.2a8 8 0 0 1 0-8.4M8 14.2a4.2 4.2 0 0 1 0-4.4M16 9.8a4.2 4.2 0 0 1 0 4.4M19 7.8a8 8 0 0 1 0 8.4"/><circle cx="12" cy="12" r="1.3" fill="currentColor" stroke="none"/>',
  "sidebar-close": '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16M14.5 9 11 12l3.5 3"/>',
  "sidebar-open": '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16M12.5 9 16 12l-3.5 3"/>',
  sun: '<circle cx="12" cy="12" r="4"/><path d="M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M17 17l1.4 1.4M5.6 18.4 7 17M17 7l1.4-1.4"/>',
  moon: '<path d="M21 14.5A8.5 8.5 0 1 1 9.5 3a7 7 0 0 0 11.5 11.5z"/>',
  lock: '<rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V8a4 4 0 0 1 8 0v3"/>',
  disconnect: '<path d="M9 6H5a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h4M16 16l4-4-4-4M20 12H9"/>',
  minimize: '<path d="M5 12h14"/>',
  maximize: '<rect x="5" y="5" width="14" height="14" rx="1.5"/>',
  restore: '<rect x="8" y="4" width="12" height="12" rx="1.5"/><path d="M8 8H5.5A1.5 1.5 0 0 0 4 9.5v9A1.5 1.5 0 0 0 5.5 20h9a1.5 1.5 0 0 0 1.5-1.5V16"/>',
  close: '<path d="M7 7l10 10M17 7 7 17"/>',
  chevron: '<path d="m6 9 6 6 6-6"/>',
  refresh: '<path d="M21 12a9 9 0 1 1-2.6-6.3M21 4v5h-5"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  subscribe: '<path d="M12 4v10M8 10l4 4 4-4M5 19h14"/>',
  search: '<circle cx="11" cy="11" r="6"/><path d="m20 20-3.5-3.5"/>',
  send: '<path d="M12 19V6M6 11l6-6 6 6"/>',
  settings: '<circle cx="12" cy="12" r="3"/><path d="M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M17 17l1.4 1.4M18.4 5.6 17 7M7 17l-1.4 1.4"/>',
  ticker: '<path d="M4 8h16M4 12h16M4 16h16"/><circle cx="8" cy="8" r="1.7" fill="currentColor" stroke="none"/><circle cx="15" cy="12" r="1.7" fill="currentColor" stroke="none"/><circle cx="11" cy="16" r="1.7" fill="currentColor" stroke="none"/>',
  shield: '<path d="M12 3 19 6v6c0 4.5-3 7.5-7 9-4-1.5-7-4.5-7-9V6z"/>',
  person: '<circle cx="12" cy="8" r="3.5"/><path d="M5 19.5c1.2-3.2 3.5-4.8 7-4.8s5.8 1.6 7 4.8"/>',
  robot: '<rect x="5" y="8" width="14" height="10" rx="2"/><path d="M12 8V4.5M9 4.5h6M9 13h.01M15 13h.01M8 18v2M16 18v2"/>',
  device: '<rect x="7" y="3" width="10" height="18" rx="2"/><path d="M11 18h2"/>',
  vehicle: '<path d="M4 15h16l-1.6-5H6.4L4 15z"/><circle cx="7.5" cy="17.5" r="1.5"/><circle cx="16.5" cy="17.5" r="1.5"/><path d="m6.2 10 1.6-4h8.4l1.6 4"/>',
  agent: '<rect x="5" y="8" width="14" height="10" rx="2"/><path d="M12 8V5M9 13h.01M15 13h.01"/><circle cx="12" cy="4" r="1" fill="currentColor" stroke="none"/>',
  compute: '<path d="M13 2 5 13h6l-1 9 9-13h-6l0-7z"/>',
  file: '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/>',
  edit: '<path d="M12 20h9M16.5 3.5a2.1 2.1 0 0 1 3 3L8 18l-4 1 1-4z"/>',
  copy: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M4 16V6a2 2 0 0 1 2-2h10"/>',
};

function nmIcon(name) {
  const body = NM_ICON_PATHS[name] || NM_ICON_PATHS.file;
  return `<svg class="nm-ico" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${body}</svg>`;
}

function paintIcons(root) {
  (root || document).querySelectorAll("[data-ico]").forEach((el) => {
    el.innerHTML = nmIcon(el.getAttribute("data-ico"));
  });
}

window.nmIcon = nmIcon;
window.paintIcons = paintIcons;
paintIcons();
