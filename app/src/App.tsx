import { useEffect, useMemo, useState } from "react";
import {
  Alert,
  App as AntApp,
  Badge,
  Button,
  Card,
  Col,
  Collapse,
  ConfigProvider,
  Empty,
  Flex,
  Form,
  Input,
  InputNumber,
  List,
  Modal,
  Popconfirm,
  Progress,
  Row,
  Select,
  Space,
  Statistic,
  Switch,
  Table,
  Tag,
  Typography,
} from "antd";
import type { TableColumnsType } from "antd";
import { ArrowLeftOutlined } from "@ant-design/icons";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { save } from "@tauri-apps/plugin-dialog";
import "./App.css";
import { checkForUpdates } from "./updater";

const { Text, Title, Link } = Typography;

type ScanConfig = {
  baseUrl: string;
  authorPath: string;
  startId: number | null;
  endId: number | null;
  usernameSelector: string;
  groupSelector: string;
  groupAttribute: string;
  memberKeyword: string;
  timeoutSecs: number;
  retries: number;
  concurrency: number;
  verifyTls: boolean;
  testWeakLogin: boolean;
  loginPath: string;
  usernameField: string;
  passwordField: string;
  rememberField: string;
  rememberValue: string;
  actionField: string;
  actionValue: string;
  failureJsonField: string;
  failureJsonValue: string;
};

type SiteRecord = {
  id: number;
  baseUrl: string;
  config: ScanConfig;
  createdAt: string;
  updatedAt: string;
};

type MemberResult = {
  id: number;
  url: string;
  status: "matched" | "skipped" | "notFound" | "error";
  username: string;
  group: string;
  loginStatus: "vulnerable" | "failed" | "error" | null;
  message: string;
};

type ScanProgress = {
  processed: number;
  total: number;
  matched: number;
  result: MemberResult;
};

type ScanSummary = {
  processed: number;
  total: number;
  matched: number;
  stopped: boolean;
  elapsedMs: number;
};

type ScanHistoryRecord = {
  id: number;
  siteId: number;
  startedAt: string;
  finishedAt: string | null;
  total: number;
  processed: number;
  matched: number;
  stopped: boolean;
  elapsedMs: number;
};

type SelectorTestResult = {
  url: string;
  count: number;
  value: string;
};

const initialConfig: ScanConfig = {
  baseUrl: "",
  authorPath: "/author/{id}",
  startId: null,
  endId: null,
  usernameSelector: "",
  groupSelector: "",
  groupAttribute: "",
  memberKeyword: "",
  timeoutSecs: 10,
  retries: 2,
  concurrency: 8,
  verifyTls: false,
  testWeakLogin: true,
  loginPath: "/wp-admin/admin-ajax.php",
  usernameField: "username",
  passwordField: "password",
  rememberField: "remember",
  rememberValue: "forever",
  actionField: "action",
  actionValue: "user_signin",
  failureJsonField: "error",
  failureJsonValue: "1",
};

const statusText: Record<MemberResult["status"], string> = {
  matched: "已命中",
  skipped: "未命中",
  notFound: "无用户",
  error: "异常",
};

const loginText: Record<string, string> = {
  vulnerable: "弱口令可登录",
  failed: "登录失败",
  error: "测试异常",
};

function AuditApp() {
  const { message, modal } = AntApp.useApp();
  const [config, setConfig] = useState<ScanConfig>(initialConfig);
  const [sites, setSites] = useState<SiteRecord[]>([]);
  const [sitesLoading, setSitesLoading] = useState(true);
  const [selectedSite, setSelectedSite] = useState<SiteRecord | null>(null);
  const [createOpen, setCreateOpen] = useState(false);
  const [newDomain, setNewDomain] = useState("http://localhost");
  const [creating, setCreating] = useState(false);
  const [saving, setSaving] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [history, setHistory] = useState<ScanHistoryRecord[]>([]);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [historyModalOpen, setHistoryModalOpen] = useState(false);
  const [historyResults, setHistoryResults] = useState<MemberResult[]>([]);
  const [selectedHistory, setSelectedHistory] = useState<ScanHistoryRecord | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [pickerField, setPickerField] = useState<"usernameSelector" | "groupSelector">("usernameSelector");
  const [pickerId, setPickerId] = useState<number | null>(null);
  const [testingSelector, setTestingSelector] = useState<"usernameSelector" | "groupSelector" | null>(null);
  const [running, setRunning] = useState(false);
  const [processed, setProcessed] = useState(0);
  const [total, setTotal] = useState(0);
  const [matched, setMatched] = useState(0);
  const [results, setResults] = useState<MemberResult[]>([]);
  const [logs, setLogs] = useState<MemberResult[]>([]);
  const [summary, setSummary] = useState<ScanSummary | null>(null);
  const [error, setError] = useState("");
  const [resultFilter, setResultFilter] = useState("all");

  useEffect(() => {
    const unlisten = listen<ScanProgress>("scan-progress", ({ payload }) => {
      setProcessed(payload.processed);
      setTotal(payload.total);
      setMatched(payload.matched);
      setLogs((current) => [payload.result, ...current].slice(0, 120));
      if (payload.result.status === "matched") {
        setResults((current) => [...current, payload.result]);
      }
    });
    return () => {
      void unlisten.then((dispose) => dispose());
    };
  }, []);

  // 启动后静默检查更新
  useEffect(() => {
    void checkForUpdates(message, modal, true);
  }, [message, modal]);

  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const handleCheckUpdate = async () => {
    setCheckingUpdate(true);
    try {
      await checkForUpdates(message, modal, false);
    } finally {
      setCheckingUpdate(false);
    }
  };

  useEffect(() => {
    const unlisten = listen<{ field: "usernameSelector" | "groupSelector"; selector: string }>(
      "selector-picked",
      ({ payload }) => {
        update(payload.field, payload.selector);
        message.success("CSS Selector 已自动填入");
      },
    );
    return () => {
      void unlisten.then((dispose) => dispose());
    };
  }, []);

  useEffect(() => {
    void loadSites();
  }, []);

  const progress = total ? Math.min(100, (processed / total) * 100) : 0;
  const weakCount = results.filter(
    (item) => item.loginStatus === "vulnerable",
  ).length;
  const visibleResults = useMemo(
    () =>
      resultFilter === "all"
        ? results
        : results.filter((item) => item.loginStatus === resultFilter),
    [results, resultFilter],
  );

  function update<K extends keyof ScanConfig>(key: K, value: ScanConfig[K]) {
    setConfig((current) => ({ ...current, [key]: value }));
    setDirty(true);
  }

  function requestSelectorPicker(field: "usernameSelector" | "groupSelector") {
    setPickerField(field);
    setPickerId(config.startId ?? 1);
    setPickerOpen(true);
  }

  async function launchSelectorPicker() {
    if (pickerId === null) {
      message.warning("请填写示例会员 ID");
      return;
    }
    try {
      await invoke("open_selector_picker", {
        baseUrl: config.baseUrl,
        authorPath: config.authorPath,
        sampleId: pickerId,
        field: pickerField,
      });
      setPickerOpen(false);
      message.info("请在打开的页面中点击需要提取的元素");
    } catch (reason) {
      message.error(String(reason));
    }
  }

  async function testSelector(field: "usernameSelector" | "groupSelector") {
    setTestingSelector(field);
    try {
      const result = await invoke<SelectorTestResult>("test_selector", { config, field });
      modal.info({
        title: result.count ? "Selector 测试成功" : "没有匹配到元素",
        width: 560,
        okText: "确定",
        content: (
          <Space direction="vertical" size={8} style={{ width: "100%", marginTop: 12 }}>
            <Text>匹配数量：<Text strong>{result.count}</Text></Text>
            <Text type="secondary">测试页面：{result.url}</Text>
            <div>
              <Text type="secondary">首个元素获取值：</Text>
              <Input.TextArea value={result.value || "（空值）"} readOnly autoSize={{ minRows: 2, maxRows: 6 }} />
            </div>
          </Space>
        ),
      });
    } catch (reason) {
      message.error(String(reason));
    } finally {
      setTestingSelector(null);
    }
  }

  async function loadSites() {
    setSitesLoading(true);
    try {
      setSites(await invoke<SiteRecord[]>("list_sites"));
    } catch (reason) {
      message.error(String(reason));
    } finally {
      setSitesLoading(false);
    }
  }

  function openSite(site: SiteRecord) {
    setSelectedSite(site);
    setConfig(site.config);
    setDirty(false);
    setError("");
    setResults([]);
    setLogs([]);
    setSummary(null);
    setProcessed(0);
    setTotal(0);
    setMatched(0);
    void loadScanHistory(site.id);
  }

  async function loadScanHistory(siteId = selectedSite?.id) {
    if (!siteId) return;
    setHistoryLoading(true);
    try {
      setHistory(await invoke<ScanHistoryRecord[]>("list_scan_history", { siteId }));
    } catch (reason) {
      message.error(String(reason));
    } finally {
      setHistoryLoading(false);
    }
  }

  async function openHistory(record: ScanHistoryRecord) {
    setSelectedHistory(record);
    setHistoryModalOpen(true);
    setHistoryResults([]);
    try {
      setHistoryResults(await invoke<MemberResult[]>("get_scan_results", { runId: record.id }));
    } catch (reason) {
      message.error(String(reason));
    }
  }

  async function removeHistory(runId: number) {
    try {
      await invoke("delete_scan_history", { runId });
      message.success("历史记录已删除");
      await loadScanHistory();
    } catch (reason) {
      message.error(String(reason));
    }
  }

  async function createSite() {
    setCreating(true);
    try {
      const site = await invoke<SiteRecord>("create_site", { baseUrl: newDomain });
      setCreateOpen(false);
      setNewDomain("http://localhost");
      await loadSites();
      openSite(site);
      message.success("站点创建成功");
    } catch (reason) {
      message.error(String(reason));
    } finally {
      setCreating(false);
    }
  }

  async function saveSite(showMessage = true) {
    if (!selectedSite) return false;
    setSaving(true);
    try {
      await invoke("update_site", { id: selectedSite.id, config });
      setSelectedSite((current) => current ? { ...current, baseUrl: config.baseUrl, config } : current);
      setDirty(false);
      if (showMessage) message.success("站点配置已保存");
      return true;
    } catch (reason) {
      setError(String(reason));
      return false;
    } finally {
      setSaving(false);
    }
  }

  async function backToSites() {
    if (dirty && !(await saveSite(false))) return;
    setSelectedSite(null);
    await loadSites();
  }

  async function removeSite(id: number) {
    try {
      await invoke("delete_site", { id });
      message.success("站点已删除");
      await loadSites();
    } catch (reason) {
      message.error(String(reason));
    }
  }

  async function startScan() {
    if (!selectedSite) return;
    if (dirty && !(await saveSite(false))) return;
    setError("");
    setResults([]);
    setLogs([]);
    setSummary(null);
    setProcessed(0);
    setMatched(0);
    setTotal(
      config.startId === null || config.endId === null
        ? 0
        : Math.max(0, config.endId - config.startId + 1),
    );
    setRunning(true);
    try {
      const value = await invoke<ScanSummary>("start_scan", {
        siteId: selectedSite.id,
        config,
      });
      setSummary(value);
      await loadScanHistory(selectedSite.id);
      message.success(value.stopped ? "扫描任务已停止" : "扫描完成");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setRunning(false);
    }
  }

  async function stopScan() {
    await invoke("stop_scan");
  }

  function csvContents() {
    const quote = (value: unknown) =>
      `"${String(value ?? "").replace(/"/g, '""')}"`;
    const header = ["ID", "用户名", "用户组", "登录检测", "作者页", "说明"];
    const rows = results.map((item) => [
      item.id,
      item.username,
      item.group,
      item.loginStatus ? loginText[item.loginStatus] : "未检测",
      item.url,
      item.message,
    ]);
    return `\uFEFF${[header, ...rows].map((row) => row.map(quote).join(",")).join("\n")}`;
  }

  async function exportResults() {
    if (!results.length) return;
    const path = await save({
      defaultPath: `zibll-audit-${new Date().toISOString().slice(0, 10)}.csv`,
      filters: [{ name: "CSV 表格", extensions: ["csv"] }],
    });
    if (!path) return;
    try {
      await invoke("save_results", { path, contents: csvContents() });
      message.success("CSV 已导出");
    } catch (reason) {
      setError(String(reason));
    }
  }

  const resultColumns: TableColumnsType<MemberResult> = [
    {
      title: "ID",
      dataIndex: "id",
      width: 76,
      render: (id, record) => (
        <Link href={record.url} target="_blank">
          #{id}
        </Link>
      ),
    },
    { title: "用户名", dataIndex: "username", width: 150, ellipsis: true },
    {
      title: "用户组",
      dataIndex: "group",
      width: 160,
      ellipsis: true,
      render: (value) => value || "—",
    },
    {
      title: "登录检测",
      dataIndex: "loginStatus",
      width: 130,
      render: (value) => {
        if (!value) return <Tag>未检测</Tag>;
        const color =
          value === "vulnerable"
            ? "error"
            : value === "failed"
              ? "success"
              : "warning";
        return <Tag color={color}>{loginText[value]}</Tag>;
      },
    },
    { title: "状态说明", dataIndex: "message", ellipsis: true },
  ];

  const numberValue = (value: number | null, fallback: number) =>
    value ?? fallback;

  const siteColumns: TableColumnsType<SiteRecord> = [
    {
      title: "站点域名",
      dataIndex: "baseUrl",
      render: (value) => <Text strong>{value}</Text>,
    },
    {
      title: "更新时间",
      dataIndex: "updatedAt",
      width: 190,
      render: (value) => <Text type="secondary">{value}</Text>,
    },
    {
      title: "操作",
      width: 150,
      render: (_, site) => (
        <Space onClick={(event) => event.stopPropagation()}>
          <Button type="link" onClick={() => openSite(site)}>配置</Button>
          <Popconfirm
            title="删除站点"
            description={`确定删除 ${site.baseUrl} 吗？`}
            okText="删除"
            cancelText="取消"
            okButtonProps={{ danger: true }}
            onConfirm={() => removeSite(site.id)}
          >
            <Button type="link" danger>删除</Button>
          </Popconfirm>
        </Space>
      ),
    },
  ];

  const historyColumns: TableColumnsType<ScanHistoryRecord> = [
    {
      title: "扫描时间",
      dataIndex: "startedAt",
      width: 180,
    },
    {
      title: "进度",
      width: 130,
      render: (_, record) => `${record.processed} / ${record.total}`,
    },
    {
      title: "命中数量",
      dataIndex: "matched",
      width: 100,
    },
    {
      title: "状态",
      width: 100,
      render: (_, record) => {
        if (!record.finishedAt) return <Tag color="warning">未完成</Tag>;
        return record.stopped ? <Tag>已停止</Tag> : <Tag color="success">已完成</Tag>;
      },
    },
    {
      title: "用时",
      width: 100,
      render: (_, record) => `${(record.elapsedMs / 1000).toFixed(1)} 秒`,
    },
    {
      title: "操作",
      width: 150,
      render: (_, record) => (
        <Space>
          <Button type="link" onClick={() => openHistory(record)}>查看结果</Button>
          <Popconfirm
            title="删除历史记录"
            description="该次扫描的所有命中结果也会一并删除。"
            okText="删除"
            cancelText="取消"
            okButtonProps={{ danger: true }}
            onConfirm={() => removeHistory(record.id)}
          >
            <Button type="link" danger>删除</Button>
          </Popconfirm>
        </Space>
      ),
    },
  ];

  return (
    <main className="app-shell">
      <header className="topbar">
        {selectedSite && (
          <Button
            type="text"
            icon={<ArrowLeftOutlined />}
            aria-label="返回站点列表"
            title="返回站点列表"
            onClick={backToSites}
          />
        )}
        <div className="brand-copy">
          <Title level={5}>{selectedSite ? selectedSite.baseUrl : "站点列表"}</Title>
          <Text type="secondary">
            {selectedSite ? "站点扫描与接口配置" : "管理需要审计的 WordPress 站点"}
          </Text>
        </div>
        <Badge
          className="run-state"
          status={running ? "processing" : "default"}
          text={running ? "扫描中" : "就绪"}
        />
        <Button
          size="small"
          loading={checkingUpdate}
          onClick={handleCheckUpdate}
          style={{ marginLeft: 12 }}
        >
          检查更新
        </Button>
      </header>

      {!selectedSite ? (
        <section className="site-list-page">
          <Flex justify="space-between" align="center" className="site-list-heading">
            <div>
              <Title level={3}>站点列表</Title>
              <Text type="secondary">点击站点进入对应的配置和扫描页面</Text>
            </div>
            <Button type="primary" onClick={() => setCreateOpen(true)}>创建站点</Button>
          </Flex>
          <Card>
            <Table<SiteRecord>
              rowKey="id"
              columns={siteColumns}
              dataSource={sites}
              loading={sitesLoading}
              pagination={false}
              locale={{
                emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="暂无站点，请先创建站点" />,
              }}
              onRow={(site) => ({
                onClick: () => openSite(site),
                className: "site-row",
              })}
            />
          </Card>
          <Modal
            title="创建站点"
            open={createOpen}
            okText="创建"
            cancelText="取消"
            confirmLoading={creating}
            onOk={createSite}
            onCancel={() => setCreateOpen(false)}
            destroyOnHidden
          >
            <Form layout="vertical" requiredMark={false}>
              <Form.Item label="站点域名" extra="创建后可继续填写页面 Selector 和扫描范围">
                <Input
                  value={newDomain}
                  onChange={(event) => setNewDomain(event.target.value)}
                  placeholder="http://localhost"
                  onPressEnter={createSite}
                  autoFocus
                />
              </Form.Item>
            </Form>
          </Modal>
        </section>
      ) : (
      <section className="workspace">
        <aside className="settings-panel">
          <Flex justify="space-between" align="center" className="panel-title">
            <Text strong>扫描配置</Text>
            <Space>
              {dirty && <Text type="warning">未保存</Text>}
              <Button size="small" loading={saving} disabled={!dirty || running} onClick={() => saveSite()}>保存</Button>
            </Space>
          </Flex>

          <Form layout="vertical" requiredMark={false} disabled={running}>
            <Form.Item label="站点域名">
              <Input
                value={config.baseUrl}
                onChange={(event) => update("baseUrl", event.target.value)}
                placeholder="https://example.com"
                allowClear
              />
            </Form.Item>
            <Form.Item
              label="作者页路径"
              extra={
                <>
                  使用 <code>{"{id}"}</code> 代表会员 ID
                </>
              }
            >
              <Input
                value={config.authorPath}
                onChange={(event) => update("authorPath", event.target.value)}
                placeholder="/author/{id}"
              />
            </Form.Item>

            <Row gutter={12}>
              <Col span={12}>
                <Form.Item label="起始 ID">
                  <InputNumber
                    min={0}
                    precision={0}
                    value={config.startId}
                    placeholder="起始 ID"
                    onChange={(value) => update("startId", value)}
                  />
                </Form.Item>
              </Col>
              <Col span={12}>
                <Form.Item label="结束 ID">
                  <InputNumber
                    min={0}
                    precision={0}
                    value={config.endId}
                    placeholder="结束 ID"
                    onChange={(value) => update("endId", value)}
                  />
                </Form.Item>
              </Col>
            </Row>

            <div className="form-divider" />

            <Form.Item
              label={
                <Space>
                  <span>用户名 CSS Selector</span>
                  <Button type="link" size="small" onClick={() => requestSelectorPicker("usernameSelector")}>
                    从页面选取
                  </Button>
                  <Button
                    type="link"
                    size="small"
                    loading={testingSelector === "usernameSelector"}
                    onClick={() => testSelector("usernameSelector")}
                  >
                    测试获取
                  </Button>
                </Space>
              }
            >
              <Input.TextArea
                className="code-input"
                value={config.usernameSelector}
                onChange={(event) =>
                  update("usernameSelector", event.target.value)
                }
                autoSize={{ minRows: 2, maxRows: 5 }}
                placeholder="输入用户名元素的 CSS Selector"
              />
            </Form.Item>
            <Form.Item
              label={
                <Space>
                  <span>用户组 CSS Selector</span>
                  <Text type="secondary" style={{ fontWeight: "normal", fontSize: 12 }}>
                    （可选，留空则只获取用户名）
                  </Text>
                  <Button type="link" size="small" onClick={() => requestSelectorPicker("groupSelector")}>
                    从页面选取
                  </Button>
                  <Button
                    type="link"
                    size="small"
                    loading={testingSelector === "groupSelector"}
                    onClick={() => testSelector("groupSelector")}
                  >
                    测试获取
                  </Button>
                </Space>
              }
            >
              <Input.TextArea
                className="code-input"
                value={config.groupSelector}
                onChange={(event) =>
                  update("groupSelector", event.target.value)
                }
                autoSize={{ minRows: 2, maxRows: 5 }}
                placeholder="留空则不获取用户组，只抓取用户名"
              />
            </Form.Item>
            <Row gutter={12}>
              <Col span={12}>
                <Form.Item label="读取属性">
                  <Input
                    value={config.groupAttribute}
                    onChange={(event) =>
                      update("groupAttribute", event.target.value)
                    }
                    placeholder="alt"
                  />
                </Form.Item>
              </Col>
              <Col span={12}>
                <Form.Item label="用户组关键词">
                  <Input
                    value={config.memberKeyword}
                    onChange={(event) =>
                      update("memberKeyword", event.target.value)
                    }
                    placeholder="留空匹配全部"
                  />
                </Form.Item>
              </Col>
            </Row>

            <Collapse
              className="advanced-collapse"
              items={[
                {
                  key: "advanced",
                  label: "高级配置",
                  children: (
                    <div className="advanced-content">
                      <Row gutter={8}>
                        <Col span={8}>
                          <Form.Item label="并发数">
                            <InputNumber
                              min={1}
                              max={64}
                              precision={0}
                              value={config.concurrency}
                              onChange={(value) =>
                                update("concurrency", numberValue(value, 1))
                              }
                            />
                          </Form.Item>
                        </Col>
                        <Col span={8}>
                          <Form.Item label="超时（秒）">
                            <InputNumber
                              min={1}
                              max={300}
                              precision={0}
                              value={config.timeoutSecs}
                              onChange={(value) =>
                                update("timeoutSecs", numberValue(value, 10))
                              }
                            />
                          </Form.Item>
                        </Col>
                        <Col span={8}>
                          <Form.Item label="重试次数">
                            <InputNumber
                              min={0}
                              max={10}
                              precision={0}
                              value={config.retries}
                              onChange={(value) =>
                                update("retries", numberValue(value, 0))
                              }
                            />
                          </Form.Item>
                        </Col>
                      </Row>

                      <Flex
                        className="switch-row"
                        align="center"
                        justify="space-between"
                      >
                        <div>
                          <Text>校验 TLS 证书</Text>
                          <Text type="secondary">自签名证书站点建议关闭</Text>
                        </div>
                        <Switch
                          checked={config.verifyTls}
                          onChange={(value) => update("verifyTls", value)}
                        />
                      </Flex>
                      <Flex
                        className="switch-row"
                        align="center"
                        justify="space-between"
                      >
                        <div>
                          <Text>检测“用户名 = 密码”</Text>
                          <Text type="secondary">仅检测命中用户组的账号</Text>
                        </div>
                        <Switch
                          checked={config.testWeakLogin}
                          onChange={(value) => update("testWeakLogin", value)}
                        />
                      </Flex>

                      {config.testWeakLogin && (
                        <Card size="small" className="login-settings">
                          <Form.Item label="登录接口路径">
                            <Input
                              className="code-input"
                              value={config.loginPath}
                              onChange={(event) =>
                                update("loginPath", event.target.value)
                              }
                            />
                          </Form.Item>
                          <Row gutter={8}>
                            <Col span={12}>
                              <Form.Item label="用户名字段">
                                <Input
                                  value={config.usernameField}
                                  onChange={(e) =>
                                    update("usernameField", e.target.value)
                                  }
                                />
                              </Form.Item>
                            </Col>
                            <Col span={12}>
                              <Form.Item label="密码字段">
                                <Input
                                  value={config.passwordField}
                                  onChange={(e) =>
                                    update("passwordField", e.target.value)
                                  }
                                />
                              </Form.Item>
                            </Col>
                            <Col span={12}>
                              <Form.Item label="Action 字段">
                                <Input
                                  value={config.actionField}
                                  onChange={(e) =>
                                    update("actionField", e.target.value)
                                  }
                                />
                              </Form.Item>
                            </Col>
                            <Col span={12}>
                              <Form.Item label="Action 值">
                                <Input
                                  value={config.actionValue}
                                  onChange={(e) =>
                                    update("actionValue", e.target.value)
                                  }
                                />
                              </Form.Item>
                            </Col>
                            <Col span={12}>
                              <Form.Item label="失败 JSON 字段">
                                <Input
                                  value={config.failureJsonField}
                                  onChange={(e) =>
                                    update("failureJsonField", e.target.value)
                                  }
                                />
                              </Form.Item>
                            </Col>
                            <Col span={12}>
                              <Form.Item label="失败判断值">
                                <Input
                                  value={config.failureJsonValue}
                                  onChange={(e) =>
                                    update("failureJsonValue", e.target.value)
                                  }
                                />
                              </Form.Item>
                            </Col>
                            <Col span={12}>
                              <Form.Item label="Remember 字段">
                                <Input
                                  value={config.rememberField}
                                  onChange={(e) =>
                                    update("rememberField", e.target.value)
                                  }
                                />
                              </Form.Item>
                            </Col>
                            <Col span={12}>
                              <Form.Item label="Remember 值">
                                <Input
                                  value={config.rememberValue}
                                  onChange={(e) =>
                                    update("rememberValue", e.target.value)
                                  }
                                />
                              </Form.Item>
                            </Col>
                          </Row>
                        </Card>
                      )}
                    </div>
                  ),
                },
              ]}
            />
          </Form>

          <div className="action-row">
            {running ? (
              <Button danger block onClick={stopScan}>
                停止扫描
              </Button>
            ) : (
              <Button type="primary" block onClick={startScan}>
                开始扫描
              </Button>
            )}
          </div>
        </aside>

        <section className="results-panel">
          <Row gutter={12} className="stats-grid">
            <Col span={8}>
              <Card>
                <Statistic
                  title="扫描进度"
                  value={processed}
                  suffix={<Text type="secondary">/ {total}</Text>}
                />
              </Card>
            </Col>
            <Col span={8}>
              <Card>
                <Statistic title="命中用户" value={matched} />
              </Card>
            </Col>
            <Col span={8}>
              <Card>
                <Statistic
                  title="弱口令命中"
                  value={weakCount}
                  valueStyle={{ color: "#ff7875" }}
                />
              </Card>
            </Col>
          </Row>

          <Card className="progress-card" size="small">
            <Flex justify="space-between" align="center">
              <Text type="secondary">
                {running
                  ? "正在读取作者页面…"
                  : summary
                    ? summary.stopped
                      ? "任务已停止"
                      : "扫描完成"
                    : "等待开始"}
              </Text>
              <Text strong>{progress.toFixed(1)}%</Text>
            </Flex>
            <Progress
              percent={progress}
              showInfo={false}
              size="small"
              status={running ? "active" : "normal"}
            />
            {summary && (
              <Text type="secondary">
                用时 {(summary.elapsedMs / 1000).toFixed(1)} 秒，完成{" "}
                {summary.processed} 个请求
              </Text>
            )}
          </Card>

          {error && (
            <Alert
              className="error-alert"
              type="error"
              message={error}
              showIcon
              closable
              onClose={() => setError("")}
            />
          )}

          <Card
            className="result-card"
            title={
              <div>
                <Text strong>命中结果</Text>
                <Text type="secondary" className="card-subtitle">
                  只展示符合用户组关键词的账号
                </Text>
              </div>
            }
            extra={
              <Space>
                <Select
                  value={resultFilter}
                  onChange={setResultFilter}
                  options={[
                    { value: "all", label: "全部结果" },
                    { value: "vulnerable", label: "弱口令可登录" },
                    { value: "failed", label: "登录失败" },
                    { value: "error", label: "测试异常" },
                  ]}
                />
                <Button onClick={exportResults} disabled={!results.length}>
                  导出 CSV
                </Button>
              </Space>
            }
          >
            <Table<MemberResult>
              rowKey={(record) => `${record.id}-${record.username}`}
              columns={resultColumns}
              dataSource={visibleResults}
              pagination={{
                pageSize: 20,
                size: "small",
                hideOnSinglePage: true,
              }}
              scroll={{ x: 760, y: 350 }}
              size="small"
              locale={{
                emptyText: (
                  <Empty
                    image={Empty.PRESENTED_IMAGE_SIMPLE}
                    description="暂无命中结果"
                  />
                ),
              }}
            />
          </Card>

          <Card
            className="history-card"
            title="扫描历史"
            extra={<Button onClick={() => loadScanHistory()} loading={historyLoading}>刷新</Button>}
          >
            <Table<ScanHistoryRecord>
              rowKey="id"
              columns={historyColumns}
              dataSource={history}
              loading={historyLoading}
              size="small"
              pagination={{ pageSize: 10, size: "small", hideOnSinglePage: true }}
              scroll={{ x: 760 }}
              locale={{
                emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="暂无扫描历史" />,
              }}
            />
          </Card>

          <Modal
            title={selectedHistory ? `扫描结果 · ${selectedHistory.startedAt}` : "扫描结果"}
            open={historyModalOpen}
            width={1000}
            footer={<Button onClick={() => setHistoryModalOpen(false)}>关闭</Button>}
            onCancel={() => setHistoryModalOpen(false)}
          >
            <Table<MemberResult>
              rowKey={(record) => `${record.id}-${record.username}`}
              columns={resultColumns}
              dataSource={historyResults}
              size="small"
              pagination={{ pageSize: 20, size: "small" }}
              scroll={{ x: 760, y: 480 }}
              locale={{
                emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="该次扫描没有命中结果" />,
              }}
            />
          </Modal>

          <Modal
            title={pickerField === "usernameSelector" ? "获取用户名 CSS Selector" : "获取用户组 CSS Selector"}
            open={pickerOpen}
            okText="打开页面选择器"
            cancelText="取消"
            onOk={launchSelectorPicker}
            onCancel={() => setPickerOpen(false)}
          >
            <Form layout="vertical" requiredMark={false}>
              <Form.Item
                label="示例会员 ID"
                extra="应用会打开对应作者页。移动鼠标预览元素，单击目标元素后 Selector 会自动填入。"
              >
                <InputNumber
                  min={0}
                  precision={0}
                  value={pickerId}
                  onChange={setPickerId}
                  style={{ width: "100%" }}
                />
              </Form.Item>
              <Alert
                type="info"
                showIcon
                message="选择 Hover 后出现的元素"
                description="先把鼠标移到触发区域，等目标文字或菜单出现后按 F 键冻结页面，再移动鼠标点击目标元素。"
              />
            </Form>
          </Modal>

          <Collapse
            className="log-collapse"
            items={[
              {
                key: "logs",
                label: (
                  <Space>
                    <Text>实时请求日志</Text>
                    <Text type="secondary">
                      {logs.length ? `最近 ${logs.length} 条` : "暂无记录"}
                    </Text>
                  </Space>
                ),
                children: (
                  <List
                    size="small"
                    dataSource={logs}
                    locale={{ emptyText: "暂无日志" }}
                    renderItem={(item) => (
                      <List.Item>
                        <Space size={10} className="log-row">
                          <Text code>#{item.id}</Text>
                          <Tag
                            color={
                              item.status === "matched"
                                ? "success"
                                : item.status === "error"
                                  ? "error"
                                  : "default"
                            }
                          >
                            {statusText[item.status]}
                          </Tag>
                          <Text type="secondary" ellipsis>
                            {item.username || item.url} · {item.message}
                          </Text>
                        </Space>
                      </List.Item>
                    )}
                  />
                ),
              },
            ]}
          />
        </section>
      </section>
      )}
    </main>
  );
}

function App() {
  return (
    <ConfigProvider>
      <AntApp>
        <AuditApp />
      </AntApp>
    </ConfigProvider>
  );
}

export default App;
