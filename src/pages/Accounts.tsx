

import {
  Calendar,
  Clock,
  Download,
  LayoutGrid,
  List,
  MoreHorizontal,
  RefreshCw,
  Search,
  Sparkles,
  ToggleLeft,
  ToggleRight,
  Trash2,
  Upload,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import AccountDetailsDialog from "../components/accounts/AccountDetailsDialog";
import AccountGrid from "../components/accounts/AccountGrid";
import AccountTable from "../components/accounts/AccountTable";
import AddAccountDialog from "../components/accounts/AddAccountDialog";
import DeviceFingerprintDialog from "../components/accounts/DeviceFingerprintDialog";
import ModalDialog from "../components/common/ModalDialog";
import Pagination from "../components/common/Pagination";
import { AccountPoolTabs, PageHeader } from "../components/common/ConsolePage";
import AccountErrorDialog from "../components/accounts/AccountErrorDialog";
import { showToast } from "../components/common/ToastContainer";
import { exportAccounts } from "../services/accountService";
import { useAccountStore } from "../stores/useAccountStore";
import { useConfigStore } from "../stores/useConfigStore";
import { Account } from "../types/account";
import { cn } from "../utils/cn";
import { isTauri } from "../utils/env";
import { request as invoke } from "../utils/request";
import { useTranslation } from "react-i18next";

type FilterType = "all" | "pro" | "ultra" | "free";
type ViewMode = "list" | "grid";
export type QuotaWindow = "5h" | "weekly";


function Accounts() {
  const { t, i18n } = useTranslation();
  const {
    accounts,
    currentAccount,
    fetchAccounts,
    fetchCurrentAccount,
    addAccount,
    deleteAccount,
    deleteAccounts,
    switchAccount,
    loading,
    refreshQuota,
    toggleProxyStatus,
    reorderAccounts,
    warmUpAccounts,
    warmUpAccount,
    updateAccountLabel,
  } = useAccountStore();
  const { config, showAllQuotas, toggleShowAllQuotas } = useConfigStore();

  const [searchQuery, setSearchQuery] = useState('');
  const [filter, setFilter] = useState<FilterType>('all');
  const [viewMode, setViewMode] = useState<ViewMode>(() => {
    const saved = localStorage.getItem('accounts_view_mode');
    return (saved === 'list' || saved === 'grid') ? saved : 'list';
  });

  const [quotaWindow, setQuotaWindow] = useState<QuotaWindow>(() => {
    const saved = localStorage.getItem('accounts_quota_window');
    return (saved === '5h' || saved === 'weekly') ? saved : '5h';
  });

  // Save view mode preference
  useEffect(() => {
    localStorage.setItem('accounts_view_mode', viewMode);
  }, [viewMode]);

  // Save quota window preference
  useEffect(() => {
    localStorage.setItem('accounts_quota_window', quotaWindow);
  }, [quotaWindow]);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [deviceAccount, setDeviceAccount] = useState<Account | null>(null);
  const [detailsAccount, setDetailsAccount] = useState<Account | null>(null);
  const [deleteConfirmId, setDeleteConfirmId] = useState<string | null>(null);
  const [isBatchDelete, setIsBatchDelete] = useState(false);
  const [toggleProxyConfirm, setToggleProxyConfirm] = useState<{
    accountId: string;
    enable: boolean;
  } | null>(null);
  const [isWarmupConfirmOpen, setIsWarmupConfirmOpen] = useState(false);
  const [isWarmuping, setIsWarmuping] = useState(false);
  const [refreshingIds, setRefreshingIds] = useState<Set<string>>(new Set());
  const [errorAccountId, setErrorAccountId] = useState<string | null>(null);

  const handleWarmup = async (accountId: string) => {
    setRefreshingIds((prev) => {
      const next = new Set(prev);
      next.add(accountId);
      return next;
    });
    try {
      const msg = await warmUpAccount(accountId);
      showToast(msg, "success");
    } catch (error) {
      showToast(`${t("common.error")}: ${error}`, "error");
    } finally {
      setRefreshingIds((prev) => {
        const next = new Set(prev);
        next.delete(accountId);
        return next;
      });
    }
  };

  const handleUpdateLabel = async (accountId: string, label: string) => {
    try {
      await updateAccountLabel(accountId, label);
      showToast(t('accounts.label_updated', 'Label updated'), 'success');
    } catch (error) {
      showToast(`${t('common.error')}: ${error}`, 'error');
    }
  };

  const handleWarmupAll = async () => {
    setIsWarmupConfirmOpen(false);
    setIsWarmuping(true);
    try {
      const isBatch = selectedIds.size > 0;
      if (isBatch) {
        const ids = Array.from(selectedIds);
        setRefreshingIds(new Set(ids));
        const results = await Promise.allSettled(
          ids.map((id) => warmUpAccount(id)),
        );
        let successCount = 0;
        results.forEach((r) => {
          if (r.status === "fulfilled") successCount++;
        });
        showToast(
          t("accounts.warmup_batch_triggered", { count: successCount }),
          "success",
        );
      } else {
        const msg = await warmUpAccounts();
        if (msg) {
          showToast(msg, "success");
        } else {
          showToast(
            t("accounts.warmup_all_triggered", "全量预热任务已触发"),
            "success",
          );
        }
      }
    } catch (error) {
      showToast(`${t("common.error")}: ${error}`, "error");
    } finally {
      setIsWarmuping(false);
      setRefreshingIds(new Set());
    }
  };

  const fileInputRef = useRef<HTMLInputElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const [containerSize, setContainerSize] = useState({ width: 0, height: 0 });

  useEffect(() => {
    if (!containerRef.current) return;
    const resizeObserver = new ResizeObserver((entries) => {
      for (let entry of entries) {
        setContainerSize({
          width: entry.contentRect.width,
          height: entry.contentRect.height,
        });
      }
    });
    resizeObserver.observe(containerRef.current);
    return () => resizeObserver.disconnect();
  }, []);

  // Pagination State
  const [currentPage, setCurrentPage] = useState(1);
  const [localPageSize, setLocalPageSize] = useState<number | null>(() => {
    const saved = localStorage.getItem("accounts_page_size");
    return saved ? parseInt(saved) : null;
  }); // 本地分页大小状态

  // Save page size preference
  useEffect(() => {
    if (localPageSize !== null) {
      localStorage.setItem("accounts_page_size", localPageSize.toString());
    }
  }, [localPageSize]);

  // 动态计算分页条数
  const ITEMS_PER_PAGE = useMemo(() => {
    // 优先使用本地设置的分页大小
    if (localPageSize && localPageSize > 0) {
      return localPageSize;
    }

    // 其次使用用户配置的固定值
    if (config?.accounts_page_size && config.accounts_page_size > 0) {
      return config.accounts_page_size;
    }

    // 回退到原有的动态计算逻辑
    if (!containerSize.height) return viewMode === "grid" ? 6 : 8;

    if (viewMode === "list") {
      const headerHeight = 36; // 缩深后的表头高度
      const rowHeight = 72; // 包含多行模型信息后的实际行高
      // 计算能容纳多少行, 默认最低 10 行
      const autoFitCount = Math.floor(
        (containerSize.height - headerHeight) / rowHeight,
      );
      return Math.max(10, autoFitCount);
    } else {
      const cardHeight = 300;
      const gap = 16; // gap-4

      const cols = Math.max(1, Math.floor((containerSize.width + gap) / (360 + gap)));

      const rows = Math.max(
        1,
        Math.floor((containerSize.height + gap) / (cardHeight + gap)),
      );
      return cols * rows;
    }
  }, [localPageSize, config?.accounts_page_size, containerSize, viewMode]);

  useEffect(() => {
    void Promise.all([fetchAccounts(), fetchCurrentAccount()]);
  }, [fetchAccounts, fetchCurrentAccount]);

  // Reset pagination when view mode changes to avoid empty pages or confusion
  useEffect(() => {
    setCurrentPage(1);
  }, [viewMode]);

  // 搜索过滤逻辑
  const searchedAccounts = useMemo(() => {
    if (!searchQuery) return accounts;
    const lowQuery = searchQuery.toLowerCase();
    return accounts.filter((a) => a.email.toLowerCase().includes(lowQuery));
  }, [accounts, searchQuery]);

  // 计算各筛选状态下的数量 (基于搜索结果)
  const filterCounts = useMemo(() => {
    return {
      all: searchedAccounts.length,
      pro: searchedAccounts.filter((a) =>
        a.quota?.subscription_tier?.toLowerCase().includes("pro"),
      ).length,
      ultra: searchedAccounts.filter((a) =>
        a.quota?.subscription_tier?.toLowerCase().includes("ultra"),
      ).length,
      free: searchedAccounts.filter((a) => {
        const tier = a.quota?.subscription_tier?.toLowerCase();
        return tier && !tier.includes("pro") && !tier.includes("ultra");
      }).length,
    };
  }, [searchedAccounts]);

  // 过滤和搜索最终结果
  const filteredAccounts = useMemo(() => {
    let result = searchedAccounts;

    if (filter === "pro") {
      result = result.filter((a) =>
        a.quota?.subscription_tier?.toLowerCase().includes("pro"),
      );
    } else if (filter === "ultra") {
      result = result.filter((a) =>
        a.quota?.subscription_tier?.toLowerCase().includes("ultra"),
      );
    } else if (filter === "free") {
      result = result.filter((a) => {
        const tier = a.quota?.subscription_tier?.toLowerCase();
        return tier && !tier.includes("pro") && !tier.includes("ultra");
      });
    }

    return result;
  }, [searchedAccounts, filter]);

  const totalPages = Math.max(1, Math.ceil(filteredAccounts.length / ITEMS_PER_PAGE));

  useEffect(() => {
    setCurrentPage((page) => Math.min(page, totalPages));
  }, [totalPages]);

  // Pagination Logic
  const paginatedAccounts = useMemo(() => {
    const startIndex = (Math.min(currentPage, totalPages) - 1) * ITEMS_PER_PAGE;
    return filteredAccounts.slice(startIndex, startIndex + ITEMS_PER_PAGE);
  }, [filteredAccounts, currentPage, totalPages, ITEMS_PER_PAGE]);

  const handlePageChange = (page: number) => {
    setCurrentPage(page);
  };

  // 清空选择当过滤改变 并重置分页
  useEffect(() => {
    setSelectedIds(new Set());
    setCurrentPage(1);
  }, [filter, searchQuery]);

  const handleToggleSelect = (id: string) => {
    const newSet = new Set(selectedIds);
    if (newSet.has(id)) {
      newSet.delete(id);
    } else {
      newSet.add(id);
    }
    setSelectedIds(newSet);
  };

  const handleToggleAll = () => {
    // 全选当前页的所有项
    const currentIds = paginatedAccounts.map((a) => a.id);
    const allSelected = currentIds.every((id) => selectedIds.has(id));

    const newSet = new Set(selectedIds);
    if (allSelected) {
      currentIds.forEach((id) => newSet.delete(id));
    } else {
      currentIds.forEach((id) => newSet.add(id));
    }
    setSelectedIds(newSet);
  };

  const handleAddAccount = async (email: string, refreshToken: string) => {
    await addAccount(email, refreshToken);
  };

  const [switchingAccountId, setSwitchingAccountId] = useState<string | null>(
    null,
  );

  const handleSwitch = async (accountId: string, targetIde?: string) => {
    if (loading || switchingAccountId) return;

    setSwitchingAccountId(accountId);
    console.log("[Accounts] handleSwitch called for:", accountId, "targetIde:", targetIde);
    try {
      await switchAccount(accountId, targetIde);
      showToast(t("common.success"), "success");
    } catch (error) {
      console.error("[Accounts] Switch failed:", error);
      showToast(`${t("common.error")}: ${error}`, "error");
    } finally {
      // Add a small delay for smoother UX
      setTimeout(() => {
        setSwitchingAccountId(null);
      }, 500);
    }
  };

  const handleRefresh = async (accountId: string) => {
    setRefreshingIds((prev) => {
      const next = new Set(prev);
      next.add(accountId);
      return next;
    });
    try {
      await refreshQuota(accountId);
      showToast(t("common.success"), "success");
    } catch (error) {
      showToast(`${t("common.error")}: ${error}`, "error");
    } finally {
      setRefreshingIds((prev) => {
        const next = new Set(prev);
        next.delete(accountId);
        return next;
      });
    }
  };

  const handleBatchDelete = () => {
    if (selectedIds.size === 0) return;
    setIsBatchDelete(true);
  };

  const executeBatchDelete = async () => {
    setIsBatchDelete(false);
    try {
      const ids = Array.from(selectedIds);
      console.log("[Accounts] Batch deleting:", ids);
      await deleteAccounts(ids);
      setSelectedIds(new Set());
      console.log("[Accounts] Batch delete success");
      showToast(t("common.success"), "success");
    } catch (error) {
      console.error("[Accounts] Batch delete failed:", error);
      showToast(`${t("common.error")}: ${error}`, "error");
    }
  };

  const handleDelete = (accountId: string) => {
    console.log("[Accounts] Request to delete:", accountId);
    setDeleteConfirmId(accountId);
  };

  const executeDelete = async () => {
    if (!deleteConfirmId) return;

    try {
      console.log("[Accounts] Executing delete for:", deleteConfirmId);
      await deleteAccount(deleteConfirmId);
      console.log("[Accounts] Delete success");
      showToast(t("common.success"), "success");
    } catch (error) {
      console.error("[Accounts] Delete failed:", error);
      showToast(`${t("common.error")}: ${error}`, "error");
    } finally {
      setDeleteConfirmId(null);
    }
  };

  const handleToggleProxy = (accountId: string, currentlyDisabled: boolean) => {
    setToggleProxyConfirm({ accountId, enable: currentlyDisabled });
  };

  const executeToggleProxy = async () => {
    if (!toggleProxyConfirm) return;

    try {
      await toggleProxyStatus(
        toggleProxyConfirm.accountId,
        toggleProxyConfirm.enable,
        toggleProxyConfirm.enable
          ? undefined
          : t("accounts.proxy_disabled_reason_manual"),
      );
      showToast(t("common.success"), "success");
    } catch (error) {
      console.error("[Accounts] Toggle proxy status failed:", error);
      showToast(`${t("common.error")}: ${error}`, "error");
    } finally {
      setToggleProxyConfirm(null);
    }
  };

  const handleBatchToggleProxy = async (enable: boolean) => {
    if (selectedIds.size === 0) return;

    try {
      const promises = Array.from(selectedIds).map((id) =>
        toggleProxyStatus(
          id,
          enable,
          enable ? undefined : t("accounts.proxy_disabled_reason_batch"),
        ),
      );
      await Promise.all(promises);
      showToast(
        enable
          ? t("accounts.toast.proxy_enabled", { count: selectedIds.size })
          : t("accounts.toast.proxy_disabled", { count: selectedIds.size }),
        "success",
      );
      setSelectedIds(new Set());
    } catch (error) {
      console.error("[Accounts] Batch toggle proxy status failed:", error);
      showToast(`${t("common.error")}: ${error}`, "error");
    }
  };

  const [isRefreshing, setIsRefreshing] = useState(false);
  const [isRefreshConfirmOpen, setIsRefreshConfirmOpen] = useState(false);

  const handleRefreshClick = () => {
    setIsRefreshConfirmOpen(true);
  };

  const executeRefresh = async () => {
    setIsRefreshConfirmOpen(false);
    setIsRefreshing(true);
    try {
      const isBatch = selectedIds.size > 0;
      let successCount = 0;
      let failedCount = 0;
      const details: string[] = [];

      if (isBatch) {
        // 批量刷新选中
        const ids = Array.from(selectedIds);
        setRefreshingIds(new Set(ids));

        const results = await Promise.allSettled(
          ids.map((id) => refreshQuota(id)),
        );

        results.forEach((result, index) => {
          const id = ids[index];
          const email = accounts.find((a) => a.id === id)?.email || id;
          if (result.status === "fulfilled") {
            successCount++;
          } else {
            failedCount++;
            details.push(`${email}: ${result.reason}`);
          }
        });
      } else {
        // 刷新所有
        setRefreshingIds(new Set(accounts.map((a) => a.id)));
        const stats = await useAccountStore.getState().refreshAllQuotas();
        if (stats) {
          successCount = stats.success;
          failedCount = stats.failed;
          details.push(...stats.details);
        }
      }

      if (failedCount === 0) {
        showToast(
          t("accounts.refresh_selected", { count: successCount }),
          "success",
        );
      } else {
        showToast(
          `${t("common.success")}: ${successCount}, ${t("common.error")}: ${failedCount}`,
          "warning",
        );
        // You might want to show details in a different way, but for toast, keep it simple or use a "view details" action if supported.
        // For now, simpler toast is better than a huge alert.
        if (details.length > 0) {
          console.warn("Refresh failures:", details);
        }
      }
    } catch (error) {
      showToast(`${t("common.error")}: ${error}`, "error");
    } finally {
      setIsRefreshing(false);
      setRefreshingIds(new Set());
    }
  };

  const exportAccountsToJson = async (accountsToExport: Account[]) => {
    try {
      if (accountsToExport.length === 0) {
        showToast(t("dashboard.toast.export_no_accounts"), "warning");
        return;
      }

      // 1. Get export data from API (contains refresh_token)
      const accountIds = accountsToExport.map((acc) => acc.id);
      const response = await exportAccounts(accountIds);

      if (!response.accounts || response.accounts.length === 0) {
        showToast(t("dashboard.toast.export_no_accounts"), "warning");
        return;
      }

      const exportData = response.accounts;
      const content = JSON.stringify(exportData, null, 2);
      const fileName = `antigravity_accounts_${new Date().toISOString().split("T")[0]}.json`;

      // 2. Determine Path & Export
      if (isTauri()) {
        let path: string | null = null;
        const { join } = await import("@tauri-apps/api/path");

        if (config?.default_export_path) {
          // Use default path
          path = await join(config.default_export_path, fileName);
        } else {
          // Use Native Dialog
          const { save } = await import("@tauri-apps/plugin-dialog");
          path = await save({
            filters: [
              {
                name: "JSON",
                extensions: ["json"],
              },
            ],
            defaultPath: fileName,
          });
        }

        if (!path) return; // Cancelled

        // 3. Write File
        await invoke("save_text_file", { path, content });
        showToast(`${t("common.success")} ${path}`, "success");
      } else {
        // Web 模式：使用浏览器下载
        const blob = new Blob([content], { type: "application/json" });
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = fileName;
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
        URL.revokeObjectURL(url);
        showToast(
          t("dashboard.toast.export_success", { path: fileName }),
          "success",
        );
      }
    } catch (error: any) {
      console.error("Export failed:", error);
      showToast(`${t("common.error")}: ${error}`, "error");
    }
  };

  const handleExport = () => {
    const idsToExport =
      selectedIds.size > 0
        ? Array.from(selectedIds)
        : accounts.map((a) => a.id);

    const accountsToExport = accounts.filter((a) => idsToExport.includes(a.id));
    exportAccountsToJson(accountsToExport);
  };

  const handleExportOne = (accountId: string) => {
    const account = accounts.find((a) => a.id === accountId);
    if (account) {
      exportAccountsToJson([account]);
    }
  };

  const processImportData = async (content: string) => {
    let importData: Array<{ email?: string; refresh_token?: string }>;
    try {
      importData = JSON.parse(content);
    } catch {
      showToast(t("accounts.import_invalid_format"), "error");
      return;
    }

    if (!Array.isArray(importData) || importData.length === 0) {
      showToast(t("accounts.import_invalid_format"), "error");
      return;
    }

    const validEntries = importData.filter(
      (item) =>
        item.refresh_token &&
        typeof item.refresh_token === "string" &&
        item.refresh_token.startsWith("1//"),
    );

    if (validEntries.length === 0) {
      showToast(t("accounts.import_invalid_format"), "error");
      return;
    }

    let successCount = 0;
    let failCount = 0;

    for (const entry of validEntries) {
      try {
        await addAccount(entry.email || "", entry.refresh_token!);
        successCount++;
      } catch (error) {
        console.error("Import account failed:", error);
        failCount++;
      }
      await new Promise((r) => setTimeout(r, 100));
    }

    if (failCount === 0) {
      showToast(
        t("accounts.import_success", { count: successCount }),
        "success",
      );
    } else if (successCount > 0) {
      showToast(
        t("accounts.import_partial", {
          success: successCount,
          fail: failCount,
        }),
        "warning",
      );
    } else {
      showToast(
        t("accounts.import_fail", { error: "All accounts failed to import" }),
        "error",
      );
    }
  };

  const handleImportJson = async () => {
    if (isTauri()) {
      try {
        const { open } = await import("@tauri-apps/plugin-dialog");
        const selected = await open({
          multiple: false,
          filters: [
            {
              name: "JSON",
              extensions: ["json"],
            },
          ],
        });
        if (!selected || typeof selected !== "string") return;

        const content: string = await invoke("read_text_file", {
          path: selected,
        });
        await processImportData(content);
      } catch (error) {
        console.error("Import failed:", error);
        showToast(t("accounts.import_fail", { error: String(error) }), "error");
      }
    } else {
      // Web 模式: 触发隐藏的 file input
      fileInputRef.current?.click();
    }
  };

  const handleFileChange = async (
    event: React.ChangeEvent<HTMLInputElement>,
  ) => {
    const file = event.target.files?.[0];
    if (!file) return;

    try {
      const content = await file.text();
      await processImportData(content);
    } catch (error) {
      console.error("Import failed:", error);
      showToast(t("accounts.import_fail", { error: String(error) }), "error");
    } finally {
      // 重置 input,允许重复选择同一文件
      event.target.value = "";
    }
  };

  const handleViewDetails = (accountId: string) => {
    const account = accounts.find((a) => a.id === accountId);
    if (account) {
      setDetailsAccount(account);
    }
  };
  const handleViewDevice = (accountId: string) => {
    const account = accounts.find((a) => a.id === accountId);
    if (account) {
      setDeviceAccount(account);
    }
  };

  const handleReorder = async (visibleOrder: string[]) => {
    const visibleIds = new Set(visibleOrder);
    let nextVisible = 0;
    const fullOrder = accounts.map((account) =>
      visibleIds.has(account.id) ? visibleOrder[nextVisible++] : account.id,
    );
    await reorderAccounts(fullOrder);
  };

  return (
    <div className="console-page console-page-fixed overflow-y-auto">
      <input
        ref={fileInputRef}
        type="file"
        accept=".json,application/json"
        style={{ display: "none" }}
        onChange={handleFileChange}
      />

      <PageHeader
        title={t('console.account_pool', { defaultValue: i18n.language.startsWith('zh') ? '账号池' : 'Account pool' })}
        description={t('console.google_pool_description', { defaultValue: i18n.language.startsWith('zh') ? '管理 Google 账号、配额与代理可用性。' : 'Manage Google accounts, quota, and proxy availability.' })}
        actions={<AddAccountDialog onAdd={handleAddAccount} />}
      />
      <AccountPoolTabs />
      <section className="console-panel flex-none space-y-3 sm:space-y-4">
        <div className="console-toolbar !flex-nowrap">
          <div className="relative flex-1 min-w-0">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-gray-400" />
            <input type="search" aria-label={t('accounts.search_placeholder')} placeholder={t('accounts.search_placeholder')}
              className="w-full pl-9 pr-3 py-2 text-sm bg-transparent border border-gray-200 dark:border-base-300 rounded-lg"
              value={searchQuery} onChange={event => setSearchQuery(event.target.value)} />
          </div>
          <button className="console-button shrink-0" onClick={handleRefreshClick} disabled={isRefreshing} title={selectedIds.size > 0 ? t('accounts.refresh_selected', { count: selectedIds.size }) : t('accounts.refresh_all')}>
            <RefreshCw size={16} className={isRefreshing ? 'animate-spin' : ''} />
            <span className="sr-only sm:not-sr-only">{isRefreshing ? t('common.loading') : selectedIds.size > 0 ? t('accounts.refresh_selected', { count: selectedIds.size }) : t('accounts.refresh_all')}</span>
          </button>
          <details className="relative shrink-0">
            <summary className="console-button cursor-pointer"><MoreHorizontal size={16} className="sm:hidden" /><span className="sr-only sm:not-sr-only">{t('console.more_actions', { defaultValue: i18n.language.startsWith('zh') ? '更多操作' : 'More actions' })}</span></summary>
            <div className="absolute right-0 top-full mt-2 z-30 w-64 max-w-[80vw] rounded-xl border border-gray-200 dark:border-base-300 bg-white dark:bg-base-100 shadow-lg p-2 flex flex-col gap-1">
              <button className="console-button justify-start" onClick={() => setIsWarmupConfirmOpen(true)} disabled={isWarmuping}>
                <Sparkles size={16} />{isWarmuping ? t('common.loading') : selectedIds.size > 0 ? t('accounts.warmup_selected', { count: selectedIds.size }) : t('accounts.warmup_all')}
              </button>
              <button className="console-button justify-start" onClick={handleImportJson}><Upload size={16} />{t('accounts.import_json')}</button>
              <button className="console-button justify-start" onClick={handleExport}><Download size={16} />{selectedIds.size > 0 ? t('accounts.export_selected', { count: selectedIds.size }) : t('common.export')}</button>
              <label className="flex items-center justify-between gap-2 p-2 text-sm cursor-pointer">
                {t('accounts.show_all_quotas')}
                <input type="checkbox" className="toggle toggle-xs toggle-primary" checked={showAllQuotas} onChange={toggleShowAllQuotas} />
              </label>
            </div>
          </details>
        </div>
        <div className="console-toolbar justify-between">
          <div className="grid grid-cols-4 sm:flex sm:flex-wrap gap-1 w-full sm:w-auto" aria-label={t('accounts.all')}>
            {(['all', 'pro', 'ultra', 'free'] as const).map(value => (
              <button key={value} className={cn('console-tab !px-1 sm:!px-3 !text-xs sm:!text-sm', filter === value && 'active')} aria-pressed={filter === value} onClick={() => setFilter(value)}>
                {t(`accounts.${value}`)} <span className="ml-1 text-xs tabular-nums opacity-70">{filterCounts[value]}</span>
              </button>
            ))}
          </div>
          <div className="flex flex-wrap gap-3 items-center">
            <div className="flex gap-1">
              <button className={cn('console-tab', quotaWindow === '5h' && 'active')} aria-pressed={quotaWindow === '5h'} onClick={() => setQuotaWindow('5h')} title={t('accounts.quota_window_5h')}><Clock size={14} />5H</button>
              <button className={cn('console-tab', quotaWindow === 'weekly' && 'active')} aria-pressed={quotaWindow === 'weekly'} onClick={() => setQuotaWindow('weekly')}><Calendar size={14} />{t('accounts.quota_window_weekly_short')}</button>
            </div>
            <div className="flex gap-1">
              <button className={cn('console-tab', viewMode === 'list' && 'active')} aria-pressed={viewMode === 'list'} aria-label={t('accounts.views.list')} onClick={() => setViewMode('list')}><List size={16} /></button>
              <button className={cn('console-tab', viewMode === 'grid' && 'active')} aria-pressed={viewMode === 'grid'} aria-label={t('accounts.views.grid')} onClick={() => setViewMode('grid')}><LayoutGrid size={16} /></button>
            </div>
          </div>
        </div>
        {selectedIds.size > 0 && (
          <div className="console-toolbar border-t border-gray-100 dark:border-base-300 pt-3" aria-live="polite">
            <span className="text-sm font-medium tabular-nums">{t('console.selected_accounts', { count: selectedIds.size, defaultValue: i18n.language.startsWith('zh') ? '已选 {{count}} 个账号' : '{{count}} accounts selected' })}</span>
            <button className="console-button" onClick={() => handleBatchToggleProxy(true)}><ToggleRight size={16} />{t('accounts.enable_proxy_selected', { count: selectedIds.size })}</button>
            <button className="console-button" onClick={() => handleBatchToggleProxy(false)}><ToggleLeft size={16} />{t('accounts.disable_proxy_selected', { count: selectedIds.size })}</button>
            <button className="console-button text-red-600 dark:text-red-400" onClick={handleBatchDelete}><Trash2 size={16} />{t('accounts.delete_selected', { count: selectedIds.size })}</button>
          </div>
        )}
      </section>


      {/* 账号列表内容区域 */}
      <div className="flex-1 min-h-64 min-w-0 w-full relative" ref={containerRef}>
        {viewMode === "list" ? (
          <div className="h-full bg-white dark:bg-base-100 rounded-2xl shadow-sm border border-gray-100 dark:border-base-200 flex flex-col overflow-hidden">
            <div className="flex-1 min-w-0 overflow-auto">
              <AccountTable
                accounts={paginatedAccounts}
                selectedIds={selectedIds}
                refreshingIds={refreshingIds}
                onToggleSelect={handleToggleSelect}
                onToggleAll={handleToggleAll}
                currentAccountId={currentAccount?.id || null}
                switchingAccountId={switchingAccountId}
                onSwitch={handleSwitch}
                onRefresh={handleRefresh}
                onViewDevice={handleViewDevice}
                onViewDetails={handleViewDetails}
                onExport={handleExportOne}
                onDelete={handleDelete}
                onToggleProxy={(id) =>
                  handleToggleProxy(
                    id,
                    !!accounts.find((a) => a.id === id)?.proxy_disabled,
                  )
                }
                onReorder={handleReorder}
                onWarmup={handleWarmup}
                onUpdateLabel={handleUpdateLabel}
                onViewError={(id: string) => setErrorAccountId(id)}
                quotaWindow={quotaWindow}
              />
            </div>
          </div>
        ) : (
          <div className="h-full overflow-y-auto">
            <AccountGrid
              accounts={paginatedAccounts}
              selectedIds={selectedIds}
              refreshingIds={refreshingIds}
              onToggleSelect={handleToggleSelect}
              currentAccountId={currentAccount?.id || null}
              switchingAccountId={switchingAccountId}
              onSwitch={handleSwitch}
              onRefresh={handleRefresh}
              onViewDevice={handleViewDevice}
              onViewDetails={handleViewDetails}
              onExport={handleExportOne}
              onDelete={handleDelete}
              onToggleProxy={(id) =>
                handleToggleProxy(
                  id,
                  !!accounts.find((a) => a.id === id)?.proxy_disabled,
                )
              }
              onWarmup={handleWarmup}
              onUpdateLabel={handleUpdateLabel}
              onViewError={(id: string) => setErrorAccountId(id)}
              quotaWindow={quotaWindow}
            />
          </div>
        )}
      </div>

      {/* 极简分页 - 无边框浮动样式 */}
      {filteredAccounts.length > 0 && (
        <div className="flex-none">
          <Pagination
            currentPage={currentPage}
            totalPages={totalPages}
            onPageChange={handlePageChange}
            totalItems={filteredAccounts.length}
            itemsPerPage={ITEMS_PER_PAGE}
            onPageSizeChange={(newSize) => {
              setLocalPageSize(newSize);
              setCurrentPage(1); // 重置到第一页
            }}
            pageSizeOptions={[10, 20, 50, 100]}
          />
        </div>
      )}

      <AccountDetailsDialog
        account={detailsAccount}
        onClose={() => setDetailsAccount(null)}
      />
      <DeviceFingerprintDialog
        account={deviceAccount}
        onClose={() => setDeviceAccount(null)}
      />

      <ModalDialog
        isOpen={!!deleteConfirmId || isBatchDelete}
        title={
          isBatchDelete
            ? t("accounts.dialog.batch_delete_title")
            : t("accounts.dialog.delete_title")
        }
        message={
          isBatchDelete
            ? t("accounts.dialog.batch_delete_msg", { count: selectedIds.size })
            : t("accounts.dialog.delete_msg")
        }
        type="confirm"
        confirmText={t("common.delete")}
        isDestructive={true}
        onConfirm={isBatchDelete ? executeBatchDelete : executeDelete}
        onCancel={() => {
          setDeleteConfirmId(null);
          setIsBatchDelete(false);
        }}
      />

      <ModalDialog
        isOpen={isRefreshConfirmOpen}
        title={
          selectedIds.size > 0
            ? t("accounts.dialog.batch_refresh_title")
            : t("accounts.dialog.refresh_title")
        }
        message={
          selectedIds.size > 0
            ? t("accounts.dialog.batch_refresh_msg", {
              count: selectedIds.size,
            })
            : t("accounts.dialog.refresh_msg")
        }
        type="confirm"
        confirmText={t("common.refresh")}
        isDestructive={false}
        onConfirm={executeRefresh}
        onCancel={() => setIsRefreshConfirmOpen(false)}
      />

      {toggleProxyConfirm && (
        <ModalDialog
          isOpen={!!toggleProxyConfirm}
          onCancel={() => setToggleProxyConfirm(null)}
          onConfirm={executeToggleProxy}
          title={
            toggleProxyConfirm.enable
              ? t("accounts.dialog.enable_proxy_title")
              : t("accounts.dialog.disable_proxy_title")
          }
          message={
            toggleProxyConfirm.enable
              ? t("accounts.dialog.enable_proxy_msg")
              : t("accounts.dialog.disable_proxy_msg")
          }
        />
      )}

      <ModalDialog
        isOpen={isWarmupConfirmOpen}
        title={
          selectedIds.size > 0
            ? t("accounts.dialog.batch_warmup_title", "批量手动预热")
            : t("accounts.dialog.warmup_all_title", "全量手动预热")
        }
        message={
          selectedIds.size > 0
            ? t(
              "accounts.dialog.batch_warmup_msg",
              "确定要为选中的 {{count}} 个账号立即触发预热吗？",
              { count: selectedIds.size },
            )
            : t(
              "accounts.dialog.warmup_all_msg",
              "确定要立即为所有符合条件的账号触发预热任务吗？这将向 Google 服务发送极小流量。",
            )
        }
        type="confirm"
        confirmText={t("accounts.warmup_now", "立即预热")}
        isDestructive={false}
        onConfirm={handleWarmupAll}
        onCancel={() => setIsWarmupConfirmOpen(false)}
      />


      {/* 账号错误详情弹窗 */}
      <AccountErrorDialog
        account={accounts.find(a => a.id === errorAccountId) || null}
        onClose={() => setErrorAccountId(null)}
      />
    </div>
  );
}

export default Accounts;
