# -*- coding: utf-8 -*-
"""序列体检对话框：后台逐帧测光 + 一张可勾选的异常表。

测光复用胶片条已经算好的那一份（lapse.luma 的 (mean, median, center, sharp)），
覆盖不满才自己扫一遍。判定本身在 lapse/qa.py 里，这里只画表和把勾选交回调色台。
不设模态：进度与取消都在外壳状态栏那一条，模态窗口会挡住取消按钮。
"""
import os

from PySide6 import QtCore, QtWidgets

from lapse import luma as luma_mod
from lapse import qa
from lapse.gui import job as jobs


class QaJob(jobs.Job):
    """逐帧测光 + 体检；luma_full 是调用方已经测好的那份。"""

    def __init__(self, items, luma_full=None):
        super().__init__([])
        self.items = list(items)
        self.luma_full = list(luma_full) if luma_full is not None else None

    def run(self):
        items = self.items
        n = len(items)
        if n < 2:
            self.done.emit(False, "序列太短")
            return
        try:
            vals = self.luma_full
            if vals is None or len(vals) != n:
                self.log.emit("逐帧测光 " + str(n) + " 帧")
                vals, issues = luma_mod.measure_luma(
                    items, 512,
                    on_progress=lambda d, _t: self.progress.emit(
                        0.9 * d / max(1, n), f"测光 {d}/{n}"),
                    cancel=lambda: self.stopping)
                if vals is None or self.stopping:
                    self.done.emit(False, "cancelled")
                    return
                if issues:
                    self.log.emit(
                        "%d 帧测不出亮度，会按「读不出来」列进表" % len(issues))
            found = qa.audit(items, vals)
            self.progress.emit(1.0, "体检完成")
            self.done.emit(True, {
                "files": [os.path.basename(p) for p in items],
                "findings": found,
                "total": n,
            })
        except Exception as e:
            self.log.emit(f"序列体检失败：{e}")
            self.done.emit(False, str(e))


class QaDialog(QtWidgets.QDialog):
    COLS = ("排除", "帧", "问题", "说明")

    def __init__(self, dev, items):
        super().__init__(dev.shell)
        self.dev = dev
        self.items = list(items)
        self._job = None
        self._closing = False
        self._loading = False
        self._stale = False
        self.setWindowTitle("序列体检")
        self.setWindowFlag(
            QtCore.Qt.WindowType.WindowContextHelpButtonHint, False)
        self.setModal(False)
        self.resize(720, 460)
        self.setMinimumSize(520, 320)
        self._build()
        self._start()

    # ------------------------------------------------------------------ 装配
    def _build(self):
        v = QtWidgets.QVBoxLayout(self)
        v.setContentsMargins(18, 16, 18, 14)
        v.setSpacing(10)

        title = QtWidgets.QLabel("序列体检")
        title.setObjectName("SettingsTitle")
        v.addWidget(title)
        self.lb_head = QtWidgets.QLabel(
            f"{len(self.items)} 帧 · 找坏帧、黑帧、虚焦、曝光离群、重复帧和缺口。"
            "勾选的帧在批量导出时跳过，不改原片。")
        self.lb_head.setObjectName("SettingsHint")
        self.lb_head.setWordWrap(True)
        v.addWidget(self.lb_head)

        self.table = QtWidgets.QTableWidget(0, len(self.COLS), self)
        self.table.setHorizontalHeaderLabels(self.COLS)
        self.table.verticalHeader().setVisible(False)
        self.table.setEditTriggers(
            QtWidgets.QAbstractItemView.EditTrigger.NoEditTriggers)
        self.table.setSelectionBehavior(
            QtWidgets.QAbstractItemView.SelectionBehavior.SelectRows)
        self.table.setAlternatingRowColors(True)
        self.table.setWordWrap(False)
        h = self.table.horizontalHeader()
        h.setSectionResizeMode(3, QtWidgets.QHeaderView.ResizeMode.Stretch)
        for col, width in ((0, 56), (1, 72), (2, 104)):
            h.setSectionResizeMode(col, QtWidgets.QHeaderView.ResizeMode.Fixed)
            self.table.setColumnWidth(col, width)
        self.table.itemChanged.connect(self._changed)
        self.table.itemDoubleClicked.connect(self._jump)
        v.addWidget(self.table, 1)

        self.lb_state = QtWidgets.QLabel("正在体检…")
        self.lb_state.setObjectName("SettingsHint")
        v.addWidget(self.lb_state)

        foot = QtWidgets.QHBoxLayout()
        self.btn_all = QtWidgets.QPushButton("全选")
        self.btn_all.clicked.connect(lambda: self._check_all(True))
        self.btn_none = QtWidgets.QPushButton("全不选")
        self.btn_none.clicked.connect(lambda: self._check_all(False))
        self.btn_again = QtWidgets.QPushButton("重新体检")
        self.btn_again.clicked.connect(self._start)
        self.btn_close = QtWidgets.QPushButton("关闭")
        self.btn_close.clicked.connect(self.reject)
        foot.addWidget(self.btn_all)
        foot.addWidget(self.btn_none)
        foot.addWidget(self.btn_again)
        foot.addStretch(1)
        foot.addWidget(self.btn_close)
        v.addLayout(foot)

    # ------------------------------------------------------------------ 分析
    def _start(self):
        if self._job is not None:
            return
        items = self.dev.photo_items()
        if len(items) < 2:
            self.lb_state.setText("序列太短，体检至少需要两张图片")
            return
        self.items = items
        self._stale = False
        job = QaJob(items, self.dev.browser.luma_for(items))
        self._job = job
        self.btn_again.setEnabled(False)
        self.lb_head.setText(
            f"{len(items)} 帧 · 找坏帧、黑帧、虚焦、曝光离群、重复帧和缺口。"
            "勾选的帧在批量导出时跳过，不改原片。")
        self.lb_state.setText("正在逐帧测光…")
        if not self.dev.start_job(job, self._done):
            self._job = None
            self.btn_again.setEnabled(True)
            self.lb_state.setText("已有任务在跑，先等它结束或取消")

    def _done(self, ok, res):
        self._job = None
        self.btn_again.setEnabled(True)
        if self._closing:
            self._closing = False
            super().close()
            return
        if not ok:
            self.lb_state.setText(
                "已取消体检" if res == "cancelled" else f"体检失败：{res}")
            return
        self._fill(res["findings"], res["total"])

    def _fill(self, findings, total):
        self.dev.set_qa_findings(self.items, findings)
        self._loading = True
        self.table.setRowCount(0)
        excluded = self.dev.qa_excluded()
        for i, kind, val, msg in findings:
            row = self.table.rowCount()
            self.table.insertRow(row)
            box = QtWidgets.QTableWidgetItem()
            if qa.excludable(kind):
                box.setFlags(QtCore.Qt.ItemFlag.ItemIsUserCheckable
                             | QtCore.Qt.ItemFlag.ItemIsEnabled
                             | QtCore.Qt.ItemFlag.ItemIsSelectable)
                box.setCheckState(
                    QtCore.Qt.CheckState.Checked
                    if os.path.normcase(os.path.normpath(
                        self.items[i])) in excluded
                    else QtCore.Qt.CheckState.Unchecked)
            else:
                box.setFlags(QtCore.Qt.ItemFlag.ItemIsEnabled)
                box.setText("—")
                box.setToolTip("缺的那几帧本来就不存在，无法排除")
            self.table.setItem(row, 0, box)
            frame = QtWidgets.QTableWidgetItem(
                f"{i + 1} / {total}" if i < len(self.items) else str(i + 1))
            frame.setData(QtCore.Qt.ItemDataRole.UserRole, i)
            self.table.setItem(row, 1, frame)
            self.table.setItem(row, 2, QtWidgets.QTableWidgetItem(
                qa.label(kind)))
            self.table.setItem(row, 3, QtWidgets.QTableWidgetItem(msg))
        self._loading = False
        counts = qa.count_by_kind(findings)
        if not findings:
            self.lb_state.setText(f"{total} 帧都正常，没有要排除的帧")
        else:
            self.lb_state.setText(
                "发现 %d 处：%s" % (len(findings),
                                    " · ".join("%s %d" % (qa.label(k), c)
                                               for k, c in sorted(
                                                   counts.items(),
                                                   key=lambda x: qa.KINDS.index(
                                                       x[0])))))
            self.lb_state.setToolTip(
                "亮度/清晰度离群按「左右邻居的中位数」判定，渐变和台阶不算离群")
        self._push()

    # ------------------------------------------------------------------ 勾选
    def _changed(self, item):
        if self._loading or item.column() != 0:
            return
        self._push()

    def _check_all(self, on):
        self._set_all(on)
        self._push()

    def uncheck_all(self):
        """调色台点了「清除排除」：只把勾撤掉，回写由那边自己做。"""
        self._set_all(False)

    def _set_all(self, on):
        self._loading = True
        for row in range(self.table.rowCount()):
            it = self.table.item(row, 0)
            if it is None or not (it.flags()
                                  & QtCore.Qt.ItemFlag.ItemIsUserCheckable):
                continue
            it.setCheckState(QtCore.Qt.CheckState.Checked if on
                             else QtCore.Qt.CheckState.Unchecked)
        self._loading = False

    def _push(self):
        if self._stale:
            return
        paths = []
        for row in range(self.table.rowCount()):
            it = self.table.item(row, 0)
            if it is None or not (it.flags()
                                  & QtCore.Qt.ItemFlag.ItemIsUserCheckable):
                continue
            if it.checkState() == QtCore.Qt.CheckState.Checked:
                frame = self.table.item(row, 1)
                i = int(frame.data(QtCore.Qt.ItemDataRole.UserRole))
                if 0 <= i < len(self.items):
                    paths.append(self.items[i])
        self.dev.set_qa_excluded(paths)

    def _jump(self, item):
        frame = self.table.item(item.row(), 1)
        if frame is None:
            return
        i = int(frame.data(QtCore.Qt.ItemDataRole.UserRole))
        if 0 <= i < len(self.items):
            self.dev.select_path(self.items[i])

    # ------------------------------------------------------------------ 收尾
    def invalidate(self):
        """调色台换了素材：旧结论和勾选都不再对应任何帧，清空等用户重跑。"""
        if self._job is not None and self._job.isRunning():
            self._job.cancel()
        self._stale = True
        self._loading = True
        self.table.setRowCount(0)
        self._loading = False
        self.items = self.dev.photo_items()
        self.btn_again.setEnabled(True)
        self.lb_state.setText("序列变了，点「重新体检」重来一遍")

    def closeEvent(self, ev):
        if self._job is not None and self._job.isRunning():
            self._closing = True
            self.lb_state.setText("正在取消体检…")
            self._job.cancel()
            ev.ignore()
            return
        super().closeEvent(ev)

    def keyPressEvent(self, ev):
        if (ev.modifiers() & QtCore.Qt.KeyboardModifier.ControlModifier
                and ev.key() == QtCore.Qt.Key.Key_A):
            self._check_all(True)
            ev.accept()
            return
        super().keyPressEvent(ev)
