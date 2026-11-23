(() => {
/* ---------- 任务栏槽位（自 app.js 拆出） ---------- */

/* ---------- 采样项目（自 app.js 拆出） ---------- */

function pageSampling() {
  const v = S.view;
  const rt = S.runtime;
  const frag = document.createDocumentFragment();
  frag.append(h("h1", "", "采样项目"), h("p", "sub", HINTS.sampling[0]));

  frag.append(h("h2", "", "刷新频率"));
  const c0 = h("div", "card");
  c0.append(row("采样间隔", "任务栏数值与详情页的刷新频率；间隔越短占用越高",
    seg([[500, "0.5 秒"], [1000, "1 秒"], [2000, "2 秒"]], v.interval,
      (x) => patch({ sampleIntervalMs: x }))));
  frag.append(c0);

  frag.append(h("h2", "", "指标开关"));
  const c1 = h("div", "card");
  const flags = [
    ["cpuSamplingEnabled", "CPU"], ["ramSamplingEnabled", "内存"],
    ["diskSamplingEnabled", "磁盘"], ["gpuSamplingEnabled", "GPU"], ["netSamplingEnabled", "网络"],
  ];
  flags.forEach(([key, name]) => {
    c1.append(row(name, "", sw(v[key], (x) => {
      const next = { ...v, [key]: x };
      if (!["cpuSamplingEnabled", "ramSamplingEnabled", "diskSamplingEnabled", "gpuSamplingEnabled", "netSamplingEnabled"].some((k) => next[k])) {
        toast("至少要保留一个指标"); return;
      }
      patch({ [key]: x });
    })));
  });
  const th = (S.runtime && S.runtime.cpuTemp) || { valid: false, source: "" };
  c1.append(row("CPU 温度", th.valid
      ? `来源：${th.source}`
      : "没有可用的温度来源。Windows 不提供通用用户态 API：需要主板暴露 ACPI 热区（笔记本通常有），或安装 LibreHardwareMonitor / OpenHardwareMonitor 并在其中开启 WMI Provider",
    h("span", "val", th.valid ? "可用" : "不可用")));
  c1.append(row("合并相同程序", "同一 exe 的多个进程合并成一行（×N）；svchost 与 Clash 行不合并",
    sw(v.merge, (x) => patch({ mergeSamePathProcesses: x }))));
  frag.append(c1);

  frag.append(h("h2", "", "磁盘"));
  const c2 = h("div", "card");
  c2.append(row("显示方式", "任务栏与图表显示的利用率口径",
    seg([["average", "平均"], ["max", "最高"], ["specific", "指定磁盘"]], v.diskDisplay,
      (x) => patch({ diskDisplay: x }))));
  const diskOpts = rt.disks.map((d) => [d.index, `磁盘 ${d.index}${d.letters ? " (" + d.letters + ")" : ""} — ${d.name}`]);
  c2.append(row("指定磁盘", "该盘不在线时自动回落为平均值",
    sel(diskOpts.length ? diskOpts : [[0, "未检测到磁盘"]], v.diskIndex, (x) => patch({ diskDisplayIndex: parseInt(x, 10) })),
    v.diskDisplay !== "specific"));
  frag.append(c2);

  frag.append(h("h2", "", "GPU"));
  const c3 = h("div", "card");
  c3.append(row("任务栏显示", "任务栏 GPU 一栏显示的内容",
    seg([["usage", "使用率"], ["temperature", "温度"], ["both", "两者"]], v.gpuContent,
      (x) => patch({ gpuTaskbarContent: x }))));
  c3.append(row("显示方式", "多适配器时的利用率口径",
    seg([["average", "平均"], ["max", "最高"], ["specific", "指定 GPU"]], v.gpuDisplay,
      (x) => patch({ gpuDisplay: x }))));
  const gpuOpts = rt.gpus.map((g) => [g.index, `GPU ${g.index} — ${g.name}`]);
  c3.append(row("指定 GPU", "该适配器不在时回落为最高值",
    sel(gpuOpts.length ? gpuOpts : [[0, "未检测到 GPU"]], v.gpuIndex, (x) => patch({ gpuDisplayIndex: parseInt(x, 10) })),
    v.gpuDisplay !== "specific"));
  c3.append(row("温度来源", "任务栏温度取自哪个适配器",
    sel([[-1, "最高温度"]].concat(gpuOpts), v.gpuTempIndex, (x) => patch({ gpuTempIndex: parseInt(x, 10) })),
    v.gpuContent === "usage"));
  c3.append(row("温度单位", "", seg([["celsius", "摄氏度"], ["fahrenheit", "华氏度"]], v.gpuTempUnit,
    (x) => patch({ gpuTempUnit: x }))));
  frag.append(c3);

  frag.append(h("h2", "", "网络"));
  const c4 = h("div", "card");
  const nicOpts = [["", "自动（累计流量最大的已连接网卡）"]].concat(rt.adapters.map((a) => [a.id, a.name]));
  c4.append(row("适配器", "钉住后该网卡不在线会回落为自动",
    sel(nicOpts, v.netAdapterId || "", (x) => {
      const nic = rt.adapters.find((a) => a.id === x);
      patch({ netAdapterId: x || null, netAdapterName: nic ? nic.name : null });
    })));
  c4.append(row("公网 IP", "查询公网地址与公网延迟（会访问外网）",
    sw(v.publicIp, (x) => patch({ publicIpEnabled: x }))));
  c4.append(row("按进程网速", `数据源：${rt.etw ? "ETW 内核事件" : rt.srum ? "SRUM" : "不可用（需管理员）"}`,
    h("span", "pill", rt.etw || rt.srum ? "已就绪" : "不可用")));
  c4.append(row("Clash / Mihomo", "从 external-controller 取代理后的进程流量",
    sw(v.clash, (x) => patch({ clashEnabled: x }))));
  c4.append(row("控制器地址", "host:port，默认 127.0.0.1:9090",
    textInput(v.clashAddress, "127.0.0.1:9090", (x) => patch({ clashApiAddress: x || null })), !v.clash));
  c4.append(row("API 密钥", "Authorization: Bearer（可留空）",
    textInput(v.clashSecret, "", (x) => patch({ clashApiSecret: x || null })), !v.clash));
  frag.append(c4);
  return frag;
}

window.pageSampling = pageSampling;
})();
