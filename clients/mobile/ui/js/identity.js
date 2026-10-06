// ── 用户身份（多账号，类似一人多个 QQ/微信号）：身份 = 公钥；私钥种子存后端。
//    公钥列表/新建走后端命令（list_identities / create_identity）；昵称与「当前身份」存前端。
(function () {
  const NAMES_KEY = "nmspace-identity-names"; // { pubkey: 昵称 }
  const CUR_KEY = "nmspace-current-user";     // 当前选中的身份公钥

  function names() { try { return JSON.parse(localStorage.getItem(NAMES_KEY) || "{}"); } catch (_) { return {}; } }
  function saveNames(m) { try { localStorage.setItem(NAMES_KEY, JSON.stringify(m)); } catch (_) {} }
  function nameOf(pk) { return (names()[pk] || "").trim(); }
  function setName(pk, name) { const m = names(); m[pk] = (name || "").trim(); saveNames(m); }

  async function list() { try { return await NM.inv("list_identities"); } catch (_) { return []; } }
  async function create(name) {
    const pk = await NM.inv("create_identity");
    if (name) setName(pk, name);
    return pk;
  }
  function current() { try { return localStorage.getItem(CUR_KEY) || ""; } catch (_) { return ""; } }
  function setCurrent(pk) { try { localStorage.setItem(CUR_KEY, pk); } catch (_) {} }
  /** 展示名：优先昵称，否则短公钥。 */
  function label(pk) { return nameOf(pk) || (pk ? pk.slice(0, 8) + "…" : ""); }

  window.Identity = { list, create, nameOf, setName, current, setCurrent, label };
})();
