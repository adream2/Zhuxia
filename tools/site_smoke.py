"""对 12 个演示逐个做一次交互冒烟测试：切页签 → 触发动作 → 读 DOM 判断有没有反应。

用法：先 `python site/build.py --out _site`，再 `python tools/site_smoke.py`。
输出一行 key=value，全 true / 非 0 才算过；`draw.arrow=no` 这种就是某个演示挂了。
"""
import re
import subprocess
import sys
from pathlib import Path

EDGE = r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "_site"

SCRIPT = """
<div id="diag">pending</div>
<script>
function sleep(ms){return new Promise(function(r){setTimeout(r,ms)})}
function click(id){var e=document.getElementById(id); if(e) e.click(); else throw new Error('no #'+id)}
function tab(i){document.querySelectorAll('.tab')[i].click()}
(async function(){
  var o=[];
  function log(k,v){o.push(k+'='+v)}
  try{
    tab(0); await sleep(60);
    log('monitor.bars', document.querySelectorAll('#tb-bars i').length>0);

    tab(1); await sleep(60);
    var q=document.getElementById('cb-search'); q.value='web';
    q.dispatchEvent(new Event('input')); await sleep(60);
    log('clipboard.visible', document.querySelectorAll('#cb-list .cb-item:not(.hide)').length);

    tab(2); await sleep(60);
    click('cap-run'); await sleep(1200);
    log('capture.pin', document.getElementById('cap-pin').classList.contains('on'));

    tab(3); await sleep(60);
    var el=document.querySelector('#m-pick [data-el]');
    var r=el.getBoundingClientRect();
    el.dispatchEvent(new MouseEvent('mousemove',{bubbles:true,clientX:r.left+4,clientY:r.top+4}));
    await sleep(80);
    log('pick.on', document.getElementById('m-sel').classList.contains('on'));

    tab(4); await sleep(60);
    click('m-ocr-run'); await sleep(1500);
    log('ocr.lines', document.querySelectorAll('#m-out .ln.on').length);

    tab(5); await sleep(60);
    click('m-table-run'); await sleep(450);
    log('table.rows', document.querySelectorAll('#m-md div').length);

    tab(6); await sleep(60);
    document.querySelector('[data-tool=arrow]').click(); await sleep(200);
    log('draw.arrow', document.querySelector('.a-arrow').getAttribute('d') ? 'yes' : 'no');

    tab(7); await sleep(60);
    var gp=document.getElementById('gs-pad'), b=gp.getBoundingClientRect();
    function pe(t,x,y){try{gp.dispatchEvent(new PointerEvent(t,{bubbles:true,pointerId:1,clientX:x,clientY:y,button:0}))}catch(e){}}
    pe('pointerdown', b.left+40, b.top+60); pe('pointermove', b.left+80, b.top+20);
    pe('pointermove', b.left+150, b.top+10); pe('pointerup', b.left+150, b.top+10);
    await sleep(120);
    log('gesture.act', document.getElementById('gs-act').textContent.slice(0,12));
    await sleep(800);
    log('gesture.cleared', document.getElementById('gs-path').getAttribute('d')==='');

    tab(8); await sleep(60);
    click('rec-run'); await sleep(1200);
    log('record.rec', document.getElementById('rec-frame').classList.contains('rec'));

    tab(9); await sleep(60);
    document.querySelector('[data-fmt=WebP]').click(); await sleep(700);
    log('convert.out', document.getElementById('cv-out').textContent.length);

    tab(10); await sleep(60);
    log('icon.cells', document.querySelectorAll('#ic-grid .ic-cell').length);

    tab(11); await sleep(60);
    click('ib-run'); await sleep(1400);
    log('imgbed.url', document.getElementById('ib-out').textContent.indexOf('http')>=0);
  }catch(e){ o.push('ERROR '+e.message) }
  document.getElementById('diag').textContent=o.join(' ');
})();
</script>
"""


def main() -> int:
    src = (OUT / "index.html").read_text(encoding="utf-8")
    page = OUT / "_smoke.html"
    page.write_text(src.replace("</body>", SCRIPT + "</body>"), encoding="utf-8")
    out = subprocess.run(
        [EDGE, "--headless=new", "--disable-gpu", "--virtual-time-budget=15000",
         "--window-size=1280,2000", "--dump-dom", page.as_uri()],
        capture_output=True, text=True, encoding="utf-8", errors="replace",
    )
    m = re.search(r'id="diag"[^>]*>(.*?)</div>', out.stdout or "", re.S)
    print(m.group(1) if m else "没有拿到诊断结果")
    return 0


if __name__ == "__main__":
    sys.exit(main())
