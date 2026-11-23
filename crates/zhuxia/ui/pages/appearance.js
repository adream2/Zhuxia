/* ---------- 外观（自 app.js 拆出） ---------- */

function selectedDisplayIds() {
  const all = (S.runtime && S.runtime.displays) || [];
  const sel = (S.view.taskbarDisplays || []).map((d) => d.id);
  if (sel.length) return sel;
  return all.filter((d) => d.primary).map((d) => d.id);
}

function displayCard() {
  const card = h("div", "card");
  const all = (S.runtime && S.runtime.displays) || [];
  if (!all.length) {
    card.append(row("未检测到显示器", "常驻进程还没写出设备清单，稍后重开本页", h("span", "val", "—")));
    return card;
  }
  const picked = selectedDisplayIds();
  for (const d of all) {
    const on = picked.includes(d.id);
    const label = d.name || d.device || "显示器";
    const hint = [d.primary ? "主屏" : "副屏", d.taskbar ? "任务栏已开启" : "任务栏未开启"]
      .join(" · ");
    card.append(row(label, hint, sw(on, (x) => {
      let next = x ? picked.concat([d.id]) : picked.filter((id) => id !== d.id);
      if (!next.length) { toast("至少要保留一个屏幕"); return; }
      const list = all.filter((x2) => next.includes(x2.id))
        .map((x2) => ({ id: x2.id, name: x2.name || x2.device || "" }));
      patch({ taskbarDisplays: list });
    }), !d.taskbar && !d.primary));
  }
  return card;
}

function pageAppearance() {
  const v = S.view;
  const fl = v.floating;
  const frag = document.createDocumentFragment();
  frag.append(h("h1", "", "外观设置"), h("p", "sub", HINTS.appearance[0]));

  frag.append(h("h2", "", "显示密度"));
  const c1 = h("div", "card");
  c1.append(row("紧凑模式", "收窄任务栏条与悬浮卡片的项目间距",
    sw(v.compact, (x) => patch({ compactLayout: x }))));
  c1.append(row("显示分割线", "各采样项目之间的细分隔线",
    sw(v.separators, (x) => patch({ showSeparators: x }))));
  frag.append(c1);

  frag.append(h("h2", "", "任务栏位置"));
  const c2 = h("div", "card");
  c2.append(row("靠左显示", "关闭 = 贴在系统托盘左侧（推荐）",
    sw(v.onLeft, (x) => patch({ overlayOnLeft: x }))));
  c2.append(row("贴靠开始按钮", "仅居中任务栏生效", sw(v.snapToStart, (x) => patch({ overlaySnapToStart: x })), !v.onLeft));
  frag.append(c2);

  frag.append(h("h2", "", "显示在这些屏幕的任务栏上"));
  frag.append(displayCard());
  frag.append(h("p", "sub", "副屏需要开启「在此显示器上显示任务栏」，否则没有位置可贴。断开连接时该项会保留，重新接上自动恢复。"));

  frag.append(h("h2", "", "悬浮模式"));
  const c3 = h("div", "card");
  c3.append(row("悬浮模式", "从任务栏脱离，变成桌面卡片", sw(fl, (x) => patch({ floatingMode: x }))));
  c3.append(row("置顶显示", "悬浮卡片始终在最前", sw(v.floatingTopmost, (x) => patch({ floatingTopmost: x })), !fl));
  c3.append(row("贴边隐藏", "拖到屏幕边缘自动收起，鼠标碰到边缘再滑出", sw(v.floatingEdgeHide, (x) => patch({ floatingEdgeHide: x })), !fl));
  c3.append(row("全屏时隐藏", "前台全屏应用运行时自动隐匿", sw(v.floatingFullscreenHide, (x) => patch({ floatingFullscreenHide: x })), !fl));
  c3.append(row("透明度", "只影响卡片背景，文字始终不透明", slider(v.opacity, (x) => patch({ floatingOpacity: x })), !fl));
  frag.append(c3);

  frag.append(h("h2", "", "项目顺序"));
  const card = h("div", "card order");
  const order = v.itemOrder.slice();
  order.forEach((slot, i) => {
    const item = h("div", "item");
    item.draggable = true;
    item.dataset.i = i;
    item.append(h("span", "grip", "⋮⋮"), h("span", "name", SLOTS[slot]));
    const moves = h("div", "moves");
    const up = h("button", "", "↑");
    const down = h("button", "", "↓");
    up.onclick = () => swap(order, i, i - 1);
    down.onclick = () => swap(order, i, i + 1);
    moves.append(up, down);
    item.append(moves);
    item.ondragstart = (e) => { e.dataTransfer.setData("text/plain", String(i)); item.classList.add("drag"); };
    item.ondragend = () => item.classList.remove("drag");
    item.ondragover = (e) => e.preventDefault();
    item.ondrop = (e) => {
      e.preventDefault();
      const from = parseInt(e.dataTransfer.getData("text/plain"), 10);
      if (!Number.isNaN(from) && from !== i) moveTo(order, from, i);
    };
    card.append(item);
  });
  frag.append(card);

  function swap(arr, a, b) {
    if (b < 0 || b >= arr.length) return;
    [arr[a], arr[b]] = [arr[b], arr[a]];
    patch({ itemOrder: arr });
  }
  function moveTo(arr, from, to) {
    const [x] = arr.splice(from, 1);
    arr.splice(to, 0, x);
    patch({ itemOrder: arr });
  }
  return frag;
}


