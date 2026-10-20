# SyncDAQ 文档构建（在 doc/ 目录下执行）
#
# 用法:
#   make            图 + 手册 PDF
#   make diagram    状态转移图（手排版，手册用）: syncdaq_state.svg / .png
#   make d2         d2 版等价图: syncdaq_state.d2.svg
#   make manual     手册 PDF: manual.pdf
#   make clean      清理生成物
#
# 依赖: python3、firefox（headless 截图 SVG → PNG）、typst、d2
# 命令不在 PATH 里时用变量覆盖，例如:
#   make TYPST=/snap/bin/typst D2=$HOME/.local/bin/d2

PYTHON  ?= python3
FIREFOX ?= firefox
TYPST   ?= typst
D2      ?= d2

# 与 gen_state_svg.py 的 W/H 一致；PNG 按 2 倍分辨率渲染
W     := 1500
H     := 820
SCALE := 2

.PHONY: all diagram d2 manual clean

all: diagram manual

diagram: syncdaq_state.svg syncdaq_state.png

syncdaq_state.svg: gen_state_svg.py
	$(PYTHON) gen_state_svg.py

# SVG 的 width/height = 100%，视口多大就渲染多大 → headless Firefox 出 PNG
syncdaq_state.png: syncdaq_state.svg
	$(FIREFOX) --headless --screenshot "$(CURDIR)/$@" \
		--window-size=$$(( $(W) * $(SCALE) )),$$(( $(H) * $(SCALE) )) \
		"file://$(CURDIR)/syncdaq_state.svg" 2>/dev/null

# d2 版：与 gen_state_svg.py 内容一致（10 状态 / 12 转移），布局由 d2 tala 决定，不用于手册
d2: syncdaq_state.d2.svg

syncdaq_state.d2.svg: syncdaq_state.d2
	$(D2) --layout tala syncdaq_state.d2 "$@"

manual: manual.pdf

manual.pdf: manual.typ syncdaq_state.png
	$(TYPST) compile manual.typ "$@"

clean:
	rm -f syncdaq_state.svg syncdaq_state.png syncdaq_state.d2.svg manual.pdf
