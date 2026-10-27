// ── 用户身份（多账号，类似一人多个 QQ/微信号）：身份 = 公钥；私钥种子存后端。
//    公钥列表/新建走后端命令（list_identities / create_identity）；昵称与「当前身份」存前端。
//    Identity.user：当前登录用户的内存镜像（含扩展属性），程序任意处可读：
//      Identity.user.attrs.dept / Identity.attr("dept") / Identity.attrRows()
(function () {
  const NAMES_KEY = "nmspace-identity-names"; // { pubkey: 昵称 }
  const CUR_KEY = "nmspace-current-user";     // 当前选中的身份公钥
  const SVC_KEY = "nmspace-identity-svc";     // Bug 3: { pubkey: svc } 每账号各自的 home 节点服务

  // 当前用户运行时对象（登录后 / 切号后由 hydrateUser 填满；未登录时 id 为空）。
  const user = {
    id: "",
    name: "",
    attrs: Object.create(null),     // key → value（字符串），随时可读
    attrNotes: Object.create(null), // key → note
  };

  function names() { try { return JSON.parse(localStorage.getItem(NAMES_KEY) || "{}"); } catch (_) { return {}; } }
  function saveNames(m) { try { localStorage.setItem(NAMES_KEY, JSON.stringify(m)); } catch (_) {} }
  function nameOf(pk) { return (names()[pk] || "").trim(); }
  function setName(pk, name) {
    const m = names(); m[pk] = (name || "").trim(); saveNames(m);
    if (pk && pk === user.id) user.name = m[pk] || label(pk);
  }

  // Bug 3: 每个账号（公钥）记住自己连接的节点服务，避免切号时沿用上一个账号的节点（CURRENT_SVC 单槽串号）。
  function svcs() { try { return JSON.parse(localStorage.getItem(SVC_KEY) || "{}"); } catch (_) { return {}; } }
  function saveSvcs(m) { try { localStorage.setItem(SVC_KEY, JSON.stringify(m)); } catch (_) {} }
  function svcOf(pk) { try { return svcs()[pk] || null; } catch (_) { return null; } }
  function setSvc(pk, svc) { if (!pk) return; const m = svcs(); m[pk] = svc; saveSvcs(m); }

  async function list() { try { return await NM.inv("list_identities"); } catch (_) { return []; } }
  async function create(name) {
    const pk = await NM.inv("create_identity");
    if (name) setName(pk, name);
    return pk;
  }
  function current() { try { return localStorage.getItem(CUR_KEY) || ""; } catch (_) { return ""; } }
  function setCurrent(pk) {
    try { localStorage.setItem(CUR_KEY, pk || ""); } catch (_) {}
    hydrateUser(pk || "");
  }
  /** 展示名：优先昵称，否则短公钥。 */
  function label(pk) { return nameOf(pk) || (pk ? pk.slice(0, 8) + "…" : ""); }

  /** 取扩展属性值；无则 ""。 */
  function attr(key) {
    const k = String(key || "").trim();
    if (!k) return "";
    const v = user.attrs[k];
    return v == null ? "" : String(v);
  }
  /** 当前用户扩展属性行（编辑器同构）：[{key,value,note}, ...] */
  function attrRows() {
    return Object.keys(user.attrs).map((key) => ({
      key,
      value: String(user.attrs[key] == null ? "" : user.attrs[key]),
      note: String(user.attrNotes[key] == null ? "" : user.attrNotes[key]),
    }));
  }

  /**
   * 从 Profile 本地缓存把指定（或当前）用户的扩展属性灌进 Identity.user。
   * 登录后、切号、资料/扩展属性保存后调用；未解锁或无资料时清空 attrs。
   */
  function hydrateUser(pk) {
    pk = (pk || current() || "").trim();
    const attrs = Object.create(null);
    const attrNotes = Object.create(null);
    let name = "";
    if (pk) {
      name = label(pk);
      const rows = (window.Profile && typeof Profile.extAttrRows === "function")
        ? Profile.extAttrRows(pk)
        : [];
      (rows || []).forEach((r) => {
        if (!r || !r.key) return;
        attrs[r.key] = r.value != null ? String(r.value) : "";
        if (r.note) attrNotes[r.key] = String(r.note);
      });
      // 昵称：资料里的 displayName 优先于本地 names 表。
      if (window.Profile && Profile.get) {
        const p = Profile.get(pk);
        if (p && p.displayName) name = String(p.displayName).trim() || name;
      }
    }
    user.id = pk;
    user.name = name;
    user.attrs = attrs;
    user.attrNotes = attrNotes;
    return user;
  }

  function clearUser() {
    user.id = "";
    user.name = "";
    user.attrs = Object.create(null);
    user.attrNotes = Object.create(null);
    return user;
  }

  window.Identity = {
    list, create, nameOf, setName, current, setCurrent, label, svcOf, setSvc,
    user, attr, attrRows, hydrateUser, clearUser,
  };
})();
