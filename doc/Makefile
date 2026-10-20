# SyncDAQ 文档构建（在 doc/ 目录下执行）
#
# 用法:
#   make            图 + 手册 PDF
#   make diagram    状态转移图: syncdaq_state.svg
#   make d2         d2 版等价图: syncdaq_state.d2.svg
#   make manual     手册 PDF: manual.pdf
#   make clean      清理生成物
#
# 依赖: python3、typst、d2
# 命令不在 PATH 里时用变量覆盖，例如:
#   make TYPST=/snap/bin/typst D2=$HOME/.local/bin/d2
#
# 图只出 SVG（typst 直接接受 SVG，PDF 里是矢量）；不再生成 PNG 中间产物。

PYTHON ?= python3
TYPST  ?= typst
D2     ?= d2

.PHONY: all diagram d2 manual clean

all: diagram manual

diagram: syncdaq_state.svg

syncdaq_state.svg: gen_state_svg.py
	$(PYTHON) gen_state_svg.py

# d2 版：与 gen_state_svg.py 内容一致（10 状态 / 12 转移），布局由 d2 tala 决定，不用于手册
d2: syncdaq_state.d2.svg

syncdaq_state.d2.svg: syncdaq_state.d2
	$(D2) --layout tala syncdaq_state.d2 "$@"

manual: manual.pdf

manual.pdf: manual.typ syncdaq_state.svg
	$(TYPST) compile manual.typ "$@"

clean:
	rm -f syncdaq_state.svg syncdaq_state.d2.svg manual.pdf
