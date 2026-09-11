import React, { useEffect, useState, useRef, useCallback, useMemo } from 'react';
import { request as invoke } from '../utils/request';
import { useTranslation } from 'react-i18next';
import { AreaChart, Area, BarChart, Bar, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer, PieChart, Pie, Cell, Legend } from 'recharts';
import { Clock, Calendar, CalendarDays, Users, Zap, TrendingUp, RefreshCw, Cpu } from 'lucide-react';
import { PageHeader } from '../components/common/ConsolePage';

interface TokenStatsAggregated {
    period: string;
    total_input_tokens: number;
    total_output_tokens: number;
    total_cached_tokens: number;
    total_tokens: number;
    request_count: number;
    uncached_input_tokens?: number;
}

interface AccountTokenStats {
    account_email: string;
    total_input_tokens: number;
    total_output_tokens: number;
    total_cached_tokens: number;
    total_tokens: number;
    request_count: number;
}

interface ModelTokenStats {
    model: string;
    total_input_tokens: number;
    total_output_tokens: number;
    total_cached_tokens: number;
    total_tokens: number;
    request_count: number;
}

interface ModelTrendPoint {
    period: string;
    model_data: Record<string, number>;
}

interface AccountTrendPoint {
    period: string;
    account_data: Record<string, number>;
}

interface TokenStatsSummary {
    total_input_tokens: number;
    total_output_tokens: number;
    total_cached_tokens: number;
    total_tokens: number;
    total_requests: number;
    unique_accounts: number;
}

type TimeRange = 'hourly' | 'daily' | 'weekly';
type ViewMode = 'model' | 'account';

const MODEL_COLORS = [
    '#3b82f6', '#8b5cf6', '#ec4899', '#f59e0b', '#10b981',
    '#06b6d4', '#6366f1', '#f43f5e', '#84cc16', '#a855f7',
    '#14b8a6', '#f97316', '#64748b', '#0ea5e9', '#d946ef'
];

const COLORS = ['#3b82f6', '#8b5cf6', '#ec4899', '#f59e0b', '#10b981', '#06b6d4', '#6366f1', '#f43f5e'];

const formatNumber = (num: number): string => {
    if (num >= 1000000) return `${(num / 1000000).toFixed(1)}M`;
    if (num >= 1000) return `${(num / 1000).toFixed(1)}K`;
    return num.toString();
};

const shortenModelName = (model: string): string => {
    return model
        .replace('gemini-', 'g-')
        .replace('claude-', 'c-')
        .replace('-preview', '')
        .replace('-latest', '');
};

const TokenStats: React.FC = () => {
    const { t, i18n } = useTranslation();
    const [timeRange, setTimeRange] = useState<TimeRange>('daily');
    const [viewMode, setViewMode] = useState<ViewMode>('model');
    const [chartData, setChartData] = useState<TokenStatsAggregated[]>([]);
    const [accountData, setAccountData] = useState<AccountTokenStats[]>([]);
    const [modelData, setModelData] = useState<ModelTokenStats[]>([]);
    const [modelTrendData, setModelTrendData] = useState<any[]>([]);
    const [accountTrendData, setAccountTrendData] = useState<any[]>([]);
    const [allModels, setAllModels] = useState<string[]>([]);
    const [allAccounts, setAllAccounts] = useState<string[]>([]);
    const [summary, setSummary] = useState<TokenStatsSummary | null>(null);
    const [loading, setLoading] = useState(true);
    const [loadError, setLoadError] = useState(false);
    const [showAllSeries, setShowAllSeries] = useState(false);
    const [reloadSequence, setReloadSequence] = useState(0);

    const fetchGeneration = useRef(0);

    useEffect(() => {
        const generation = ++fetchGeneration.current;
        const controller = new AbortController();
        const requestOptions = { signal: controller.signal };
        const range = timeRange;
        setLoading(true);
        setLoadError(false);

        const load = async () => {
            const hours = range === 'hourly' ? 24 : range === 'daily' ? 168 : 672;
            const aggregate = range === 'hourly'
                ? invoke<TokenStatsAggregated[]>('get_token_stats_hourly', { hours }, requestOptions)
                : range === 'daily'
                    ? invoke<TokenStatsAggregated[]>('get_token_stats_daily', { days: 7 }, requestOptions)
                    : invoke<TokenStatsAggregated[]>('get_token_stats_weekly', { weeks: 4 }, requestOptions);
            const modelTrend = range === 'hourly'
                ? invoke<ModelTrendPoint[]>('get_token_stats_model_trend_hourly', { hours }, requestOptions)
                : invoke<ModelTrendPoint[]>('get_token_stats_model_trend_daily', { days: range === 'daily' ? 7 : 28 }, requestOptions);
            const accountTrend = range === 'hourly'
                ? invoke<AccountTrendPoint[]>('get_token_stats_account_trend_hourly', { hours }, requestOptions)
                : invoke<AccountTrendPoint[]>('get_token_stats_account_trend_daily', { days: range === 'daily' ? 7 : 28 }, requestOptions);

            try {
                const [data, modelTrendData, accountTrendData, accounts, models, summaryData] = await Promise.all([
                    aggregate,
                    modelTrend,
                    accountTrend,
                    invoke<AccountTokenStats[]>('get_token_stats_by_account', { hours }, requestOptions),
                    invoke<ModelTokenStats[]>('get_token_stats_by_model', { hours }, requestOptions),
                    invoke<TokenStatsSummary>('get_token_stats_summary', { hours }, requestOptions),
                ]);
                if (generation !== fetchGeneration.current) return;

                const chart = data.map(point => ({
                    ...point,
                    total_cached_tokens: point.total_cached_tokens || 0,
                    uncached_input_tokens: Math.max((point.total_input_tokens || 0) - (point.total_cached_tokens || 0), 0),
                }));
                const modelList = Array.from(new Set(modelTrendData.flatMap(point => Object.keys(point.model_data))));
                const transformedModelTrend = modelTrendData.map(point => {
                    const row: Record<string, any> = { period: point.period };
                    modelList.forEach(model => { row[model] = point.model_data[model] || 0; });
                    return row;
                });
                const accountList = Array.from(new Set(accountTrendData.flatMap(point => Object.keys(point.account_data))));
                const transformedAccountTrend = accountTrendData.map(point => {
                    const row: Record<string, any> = { period: point.period };
                    accountList.forEach(account => { row[account] = point.account_data[account] || 0; });
                    return row;
                });

                setChartData(chart);
                setAllModels(modelList);
                setModelTrendData(transformedModelTrend);
                setAllAccounts(accountList);
                setAccountTrendData(transformedAccountTrend);
                setAccountData(accounts);
                setModelData(models);
                setSummary(summaryData);
            } catch (error) {
                if (generation !== fetchGeneration.current) return;
                setLoadError(true);
                console.error('Failed to fetch token stats:', error);
            } finally {
                if (generation === fetchGeneration.current) setLoading(false);
            }
        };

        void load();
        return () => {
            fetchGeneration.current += 1;
            controller.abort();
        };
    }, [timeRange, reloadSequence]);

    const otherLabel = t('console.stats_other', { defaultValue: i18n.language.startsWith('zh') ? '其他' : 'Other' });
    const trend = useMemo(() => {
        const rows = viewMode === 'model' ? modelTrendData : accountTrendData;
        const names = viewMode === 'model' ? allModels : allAccounts;
        const totals: Record<string, number> = Object.create(null);
        names.forEach(name => { totals[name] = 0; });
        rows.forEach(row => names.forEach(name => { totals[name] += row[name] || 0; }));
        const ranked = [...names].sort((a, b) => totals[b] - totals[a]);
        const visible = showAllSeries ? ranked : ranked.slice(0, 6);
        const hidden = showAllSeries ? [] : ranked.slice(6);
        const palette = viewMode === 'model' ? MODEL_COLORS : COLORS;
        const series = visible.map((name, index) => ({ name, color: palette[index % palette.length] }));
        if (hidden.length) series.push({ name: otherLabel, color: '#94a3b8' });
        return {
            count: names.length,
            series,
            rows: rows.map(row => ({
                period: row.period,
                values: [
                    ...visible.map(name => row[name] || 0),
                    ...(hidden.length ? [hidden.reduce((sum, name) => sum + (row[name] || 0), 0)] : [])
                ]
            }))
        };
    }, [viewMode, modelTrendData, accountTrendData, allModels, allAccounts, showAllSeries, otherLabel]);

    const pieData = useMemo(() => {
        const ranked = [...accountData].sort((a, b) => b.total_tokens - a.total_tokens);
        const visible = showAllSeries ? ranked : ranked.slice(0, 6);
        const entries = visible.map((account, index) => ({
            name: account.account_email,
            value: account.total_tokens,
            fullEmail: account.account_email,
            color: COLORS[index % COLORS.length]
        }));
        if (!showAllSeries && ranked.length > 6) entries.push({
            name: otherLabel,
            value: ranked.slice(6).reduce((sum, account) => sum + account.total_tokens, 0),
            fullEmail: otherLabel,
            color: '#94a3b8'
        });
        return entries;
    }, [accountData, showAllSeries, otherLabel]);

    const trendChartContainerRef = useRef<HTMLDivElement>(null);
    const [tooltipPosition, setTooltipPosition] = useState<{ x: number; y: number } | undefined>(undefined);

    // Ref and state for pie chart tooltip position
    const pieChartContainerRef = useRef<HTMLDivElement>(null);
    const [pieTooltipPosition, setPieTooltipPosition] = useState<{ x: number; y: number } | undefined>(undefined);

    // Handle mouse move to calculate tooltip position
    const handleTrendChartMouseMove = useCallback((e: any) => {
        if (!trendChartContainerRef.current || !e?.activeCoordinate) return;

        const containerRect = trendChartContainerRef.current.getBoundingClientRect();
        const tooltipWidth = 200; // Approximate tooltip width
        const rightEdgeThreshold = containerRect.width - tooltipWidth - 20; // 20px buffer

        const mouseXInContainer = e.activeCoordinate.x;

        if (mouseXInContainer > rightEdgeThreshold) {
            setTooltipPosition({
                x: e.activeCoordinate.x - tooltipWidth - 15,
                y: e.activeCoordinate.y
            });
        } else {
            setTooltipPosition(undefined); // Use default positioning
        }
    }, []);

    // Handle mouse move for pie chart to calculate tooltip position
    const handlePieChartMouseMove = useCallback((e: any) => {
        if (!pieChartContainerRef.current) return;

        const containerRect = pieChartContainerRef.current.getBoundingClientRect();
        const tooltipWidth = 180; // Approximate tooltip width for pie chart

        // Get mouse position relative to container
        if (e?.activeCoordinate) {
            const mouseXInContainer = e.activeCoordinate.x;
            const rightEdgeThreshold = containerRect.width - tooltipWidth - 20;

            if (mouseXInContainer > rightEdgeThreshold) {
                setPieTooltipPosition({
                    x: e.activeCoordinate.x - tooltipWidth - 15,
                    y: e.activeCoordinate.y
                });
            } else {
                setPieTooltipPosition(undefined);
            }
        }
    }, []);

    // Custom Tooltip for Trend Chart
    const CustomTrendTooltip = ({ active, payload, label }: any) => {
        if (!active || !payload || !payload.length) return null;

        // Sort payload by value descending
        const sortedPayload = [...payload].sort((a: any, b: any) => b.value - a.value);

        return (
            <div className="bg-white/95 dark:bg-gray-800/95 backdrop-blur-sm p-2.5 rounded-xl shadow-xl border border-gray-100 dark:border-gray-700 text-xs z-[100] min-w-[180px] pointer-events-none">
                <p className="font-semibold text-gray-700 dark:text-gray-200 mb-1.5 border-b border-gray-100 dark:border-gray-700 pb-1.5">
                    {label}
                </p>
                <div className="max-h-[180px] overflow-y-auto space-y-1 pr-1.5 scrollbar-thin scrollbar-thumb-gray-200 dark:scrollbar-thumb-gray-700">
                    {sortedPayload.map((entry: any, index: number) => {
                        const name = entry.name;
                        const displayName = viewMode === 'model' ? shortenModelName(name) : name.split('@')[0];
                        return (
                            <div key={index} className="flex items-center justify-between gap-4">
                                <div className="flex items-center gap-2 overflow-hidden">
                                    <div className="w-2 h-2 rounded-full flex-shrink-0" style={{ backgroundColor: entry.color }} />
                                    <span className="text-gray-500 dark:text-gray-400 truncate max-w-[120px]" title={name}>
                                        {displayName}
                                    </span>
                                </div>
                                <span className="font-mono font-medium text-gray-700 dark:text-gray-200">
                                    {entry.value.toLocaleString()}
                                </span>
                            </div>
                        );
                    })}
                </div>
            </div>
        );
    };

    const UsageTrendTooltip = ({ active, payload, label }: any) => {
        if (!active || !payload || !payload.length) return null;
        const row = payload[0]?.payload || {};
        const items = [
            { label: t('token_stats.total', '合计'), value: row.total_tokens || 0, color: '#111827' },
            { label: t('token_stats.input', '输入'), value: row.total_input_tokens || 0, color: '#3b82f6' },
            { label: t('token_stats.cached_token', '缓存命中'), value: row.total_cached_tokens || 0, color: '#93c5fd' },
            { label: t('token_stats.output', '输出'), value: row.total_output_tokens || 0, color: '#8b5cf6' },
        ];
        return (
            <div className="bg-white/95 dark:bg-gray-800/95 backdrop-blur-sm p-2.5 rounded-xl shadow-xl border border-gray-100 dark:border-gray-700 text-xs z-[100] pointer-events-none min-w-[170px]">
                {label && <p className="font-semibold text-gray-700 dark:text-gray-200 mb-2">{label}</p>}
                <div className="space-y-1">
                    {items.map((item) => (
                        <div key={item.label} className="flex items-center justify-between gap-4">
                            <div className="flex items-center gap-2">
                                <div className="w-2 h-2 rounded-full" style={{ backgroundColor: item.color }} />
                                <span className="text-gray-500 dark:text-gray-400">
                                    {item.label}:
                                </span>
                            </div>
                            <span className="font-mono font-medium text-gray-700 dark:text-gray-200">
                                {formatNumber(item.value)}
                            </span>
                        </div>
                    ))}
                    <div className="flex items-center justify-between gap-4 pt-1 border-t border-gray-100 dark:border-gray-700">
                        <span className="text-gray-500 dark:text-gray-400">
                            {t('token_stats.requests', '请求数')}:
                        </span>
                        <span className="font-mono font-medium text-gray-700 dark:text-gray-200">
                            {(row.request_count || 0).toLocaleString()}
                        </span>
                    </div>
                </div>
            </div>
        );
    };

    // Custom Tooltip for Pie Chart
    const CustomPieTooltip = ({ active, payload }: any) => {
        if (!active || !payload || !payload.length) return null;
        const entry = payload[0];
        return (
            <div className="bg-white/95 dark:bg-gray-800/95 backdrop-blur-sm p-2.5 rounded-xl shadow-xl border border-gray-100 dark:border-gray-700 text-xs z-[100] pointer-events-none">
                <div className="flex items-center gap-2">
                    <div className="w-2 h-2 rounded-full" style={{ backgroundColor: entry.payload.color || entry.color }} />
                    <span className="text-gray-500 dark:text-gray-400">
                        {entry.payload.fullEmail || entry.name}:
                    </span>
                    <span className="font-mono font-medium text-gray-700 dark:text-gray-200">
                        {entry.value.toLocaleString()}
                    </span>
                </div>
            </div>
        );
    };

    return (
        <div className="console-page console-page-scroll">
            <div className="space-y-5">
                <PageHeader
                    title={t('nav.token_stats')}
                    description={t('console.stats_description', { defaultValue: i18n.language.startsWith('zh') ? '查看用量趋势、模型分布与账号明细。' : 'Explore usage trends, model distribution and account details.' })}
                    actions={<button onClick={() => setReloadSequence(sequence => sequence + 1)} disabled={loading} className="console-button">
                        <RefreshCw size={16} className={loading ? 'animate-spin' : ''} />{t('common.refresh')}
                    </button>}
                />
                {loadError && <div role="alert" className="console-panel text-error">{t('common.load_failed')}</div>}
                <div className="console-toolbar">
                        <div className="console-tabs">
                            <button
                                onClick={() => setTimeRange('hourly')}
                                aria-pressed={timeRange === 'hourly'}
                                className={`console-tab ${timeRange === 'hourly' ? 'active' : ''}`}
                            >
                                <Clock className="w-4 h-4" />
                                {t('token_stats.hourly', '小时')}
                            </button>
                            <button
                                onClick={() => setTimeRange('daily')}
                                aria-pressed={timeRange === 'daily'}
                                className={`console-tab ${timeRange === 'daily' ? 'active' : ''}`}
                            >
                                <Calendar className="w-4 h-4" />
                                {t('token_stats.daily', '日')}
                            </button>
                            <button
                                onClick={() => setTimeRange('weekly')}
                                aria-pressed={timeRange === 'weekly'}
                                className={`console-tab ${timeRange === 'weekly' ? 'active' : ''}`}
                            >
                                <CalendarDays className="w-4 h-4" />
                                {t('token_stats.weekly', '周')}
                            </button>
                        </div>
                </div>

                {summary && (
                    <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-6 gap-4">
                        <div className="console-panel">
                            <div className="flex items-center gap-2 text-gray-500 dark:text-gray-400 text-sm mb-2">
                                <div className="p-1.5 rounded-lg bg-gray-100 dark:bg-gray-700">
                                    <Zap className="w-4 h-4 text-gray-600 dark:text-gray-300" />
                                </div>
                                {t('token_stats.total_tokens', '总 Token')}
                            </div>
                            <div className="text-2xl font-bold text-gray-800 dark:text-white">
                                {formatNumber(summary.total_tokens)}
                            </div>
                        </div>
                        <div className="console-panel">
                            <div className="flex items-center gap-2 text-blue-600/80 dark:text-blue-400/80 text-sm mb-2">
                                <div className="p-1.5 rounded-lg bg-blue-100/50 dark:bg-blue-900/30">
                                    <TrendingUp className="w-4 h-4 text-blue-600 dark:text-blue-400" />
                                </div>
                                {t('token_stats.input_tokens', '输入 Token')}
                            </div>
                            <div className="text-2xl font-bold text-blue-600 dark:text-blue-400">
                                {formatNumber(summary.total_input_tokens)}
                            </div>
                        </div>
                        <div className="console-panel">
                            <div className="flex items-center gap-2 text-purple-600/80 dark:text-purple-400/80 text-sm mb-2">
                                <div className="p-1.5 rounded-lg bg-purple-100/50 dark:bg-purple-900/30">
                                    <TrendingUp className="w-4 h-4 rotate-180 text-purple-600 dark:text-purple-400" />
                                </div>
                                {t('token_stats.output_tokens', '输出 Token')}
                            </div>
                            <div className="text-2xl font-bold text-purple-600 dark:text-purple-400">
                                {formatNumber(summary.total_output_tokens)}
                            </div>
                        </div>
                        <div className="console-panel">
                            <div className="flex items-center gap-2 text-sky-600/80 dark:text-sky-400/80 text-sm mb-2">
                                <div className="p-1.5 rounded-lg bg-sky-100/50 dark:bg-sky-900/30">
                                    <Zap className="w-4 h-4 text-sky-600 dark:text-sky-400" />
                                </div>
                                {t('token_stats.cached_token', '缓存命中')}
                            </div>
                            <div className="text-2xl font-bold text-sky-600 dark:text-sky-400">
                                {formatNumber(summary.total_cached_tokens)}
                            </div>
                        </div>
                        <div className="console-panel">
                            <div className="flex items-center gap-2 text-green-600/80 dark:text-green-400/80 text-sm mb-2">
                                <div className="p-1.5 rounded-lg bg-green-100/50 dark:bg-green-900/30">
                                    <Users className="w-4 h-4 text-green-600 dark:text-green-400" />
                                </div>
                                {t('token_stats.accounts_used', '活跃账号')}
                            </div>
                            <div className="text-2xl font-bold text-green-600 dark:text-green-400">
                                {summary.unique_accounts}
                            </div>
                        </div>
                        <div className="console-panel">
                            <div className="flex items-center gap-2 text-orange-600/80 dark:text-orange-400/80 text-sm mb-2">
                                <div className="p-1.5 rounded-lg bg-orange-100/50 dark:bg-orange-900/30">
                                    <Cpu className="w-4 h-4 text-orange-600 dark:text-orange-400" />
                                </div>
                                {t('token_stats.models_used', '使用模型')}
                            </div>
                            <div className="text-2xl font-bold text-orange-600 dark:text-orange-400">
                                {modelData.length}
                            </div>
                        </div>
                    </div>
                )}

                <div className="console-panel min-w-0">
                    <div className="console-toolbar justify-between mb-4">
                        <h2 className="text-lg font-semibold text-gray-800 dark:text-white flex items-center gap-2">
                            {viewMode === 'model' ? (
                                <Cpu className="w-5 h-5 text-purple-500" />
                            ) : (
                                <Users className="w-5 h-5 text-green-500" />
                            )}
                            {viewMode === 'model'
                                ? t('token_stats.model_trend', '分模型使用趋势')
                                : t('token_stats.account_trend', '分账号使用趋势')
                            }
                        </h2>
                        <div className="console-tabs">
                            <button
                                onClick={() => setViewMode('model')}
                                aria-pressed={viewMode === 'model'}
                                className={`console-tab ${viewMode === 'model' ? 'active' : ''}`}
                            >
                                {t('token_stats.by_model', '按模型')}
                            </button>
                            <button
                                onClick={() => setViewMode('account')}
                                aria-pressed={viewMode === 'account'}
                                className={`console-tab ${viewMode === 'account' ? 'active' : ''}`}
                            >
                                {t('token_stats.by_account_view', '按账号')}
                            </button>
                        </div>
                        {(trend.count > 6 || accountData.length > 6) && <button
                            className="console-button"
                            aria-pressed={showAllSeries}
                            onClick={() => setShowAllSeries(value => !value)}
                        >
                            {showAllSeries
                                ? t('console.stats_show_top', { defaultValue: i18n.language.startsWith('zh') ? '显示前 6 项 + 其他' : 'Show top 6 + Other' })
                                : t('console.stats_show_all', { defaultValue: i18n.language.startsWith('zh') ? '显示全部系列' : 'Show all series' })}
                        </button>}
                    </div>
                    <div className="h-80 min-w-0" ref={trendChartContainerRef}>
                        {trend.rows.length > 0 && trend.series.length > 0 ? (
                            <ResponsiveContainer width="100%" height="100%">
                                <AreaChart
                                    data={trend.rows}
                                    onMouseMove={handleTrendChartMouseMove}
                                    onMouseLeave={() => setTooltipPosition(undefined)}
                                >
                                    <CartesianGrid strokeDasharray="3 3" vertical={false} stroke="#374151" strokeOpacity={0.15} />
                                    <XAxis
                                        dataKey="period"
                                        tick={{ fontSize: 11, fill: '#6b7280' }}
                                        tickFormatter={(val) => {
                                            if (timeRange === 'hourly') return val.split(' ')[1] || val;
                                            if (timeRange === 'daily') return val.split('-').slice(1).join('/');
                                            return val;
                                        }}
                                        axisLine={false}
                                        tickLine={false}
                                        dy={10}
                                    />
                                    <YAxis
                                        tick={{ fontSize: 11, fill: '#6b7280' }}
                                        tickFormatter={(val) => formatNumber(val)}
                                        axisLine={false}
                                        tickLine={false}
                                    />
                                    <Tooltip
                                        content={<CustomTrendTooltip />}
                                        cursor={{ stroke: '#6b7280', strokeWidth: 1, strokeDasharray: '4 4', fill: 'transparent' }}
                                        allowEscapeViewBox={{ x: true, y: true }}
                                        position={tooltipPosition}
                                        wrapperStyle={{ zIndex: 100 }}
                                    />
                                    <Legend
                                        formatter={(value) => <span title={value}>{viewMode === 'model' ? shortenModelName(value) : value.split('@')[0]}</span>}
                                        wrapperStyle={{
                                            fontSize: '11px',
                                            paddingTop: '10px',
                                            maxHeight: '60px',
                                            overflowY: 'auto',
                                            zIndex: 0
                                        }}
                                    />
                                    {trend.series.map((item, index) => (
                                        <Area
                                            key={index}
                                            name={item.name}
                                            type="monotone"
                                            dataKey={`values.${index}`}
                                            isAnimationActive={false}
                                            stackId="1"
                                            stroke={item.color}
                                            fill={item.color}
                                            fillOpacity={0.25}
                                        />
                                    ))}
                                </AreaChart>
                            </ResponsiveContainer>
                        ) : (
                            <div className="h-full flex items-center justify-center text-gray-400">
                                {loading ? t('common.loading', '加载中...') : t('token_stats.no_data', '暂无数据')}
                            </div>
                        )}
                    </div>
                </div>

                <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
                    <div className="console-panel lg:col-span-2 min-w-0 flex flex-col">
                        <h2 className="text-lg font-semibold text-gray-800 dark:text-white mb-4">
                            {t('token_stats.usage_trend', 'Token 使用趋势')}
                        </h2>
                        <div className="h-72 min-w-0">
                            {chartData.length > 0 ? (
                                <ResponsiveContainer width="100%" height="100%">
                                    <BarChart data={chartData}>
                                        <CartesianGrid strokeDasharray="3 3" vertical={false} stroke="#374151" strokeOpacity={0.15} />
                                        <XAxis
                                            dataKey="period"
                                            tick={{ fontSize: 11, fill: '#6b7280' }}
                                            tickFormatter={(val) => {
                                                if (timeRange === 'hourly') return val.split(' ')[1] || val;
                                                if (timeRange === 'daily') return val.split('-').slice(1).join('/');
                                                return val;
                                            }}
                                            axisLine={false}
                                            tickLine={false}
                                            dy={10}
                                        />
                                        <YAxis
                                            tick={{ fontSize: 11, fill: '#6b7280' }}
                                            tickFormatter={(val) => formatNumber(val)}
                                            axisLine={false}
                                            tickLine={false}
                                        />
                                        <Tooltip
                                            content={<UsageTrendTooltip />}
                                            cursor={{ fill: 'transparent' }}
                                            allowEscapeViewBox={{ x: true, y: true }}
                                            wrapperStyle={{ zIndex: 100 }}
                                        />
                                        <Bar isAnimationActive={false} dataKey="total_cached_tokens" name={t('token_stats.cached_token', '缓存命中')} stackId="input" fill="#93c5fd" radius={[0, 0, 4, 4]} maxBarSize={50} />
                                        <Bar isAnimationActive={false} dataKey="uncached_input_tokens" name={t('token_stats.input', '输入')} stackId="input" fill="#3b82f6" radius={[4, 4, 0, 0]} maxBarSize={50} />
                                        <Bar isAnimationActive={false} dataKey="total_output_tokens" name={t('token_stats.output', '输出')} fill="#8b5cf6" radius={[4, 4, 0, 0]} maxBarSize={50} />
                                    </BarChart>
                                </ResponsiveContainer>
                            ) : (
                                <div className="h-full flex items-center justify-center text-gray-400">
                                    {loading ? t('common.loading', '加载中...') : t('token_stats.no_data', '暂无数据')}
                                </div>
                            )}
                        </div>
                    </div>

                    <div className="console-panel min-w-0">
                        <h2 className="text-lg font-semibold text-gray-800 dark:text-white mb-4">
                            {t('token_stats.by_account', '分账号统计')}
                        </h2>
                        <div className="h-48" ref={pieChartContainerRef}>
                            {pieData.length > 0 ? (
                                <ResponsiveContainer width="100%" height="100%">
                                    <PieChart
                                        onMouseMove={handlePieChartMouseMove}
                                        onMouseLeave={() => setPieTooltipPosition(undefined)}
                                    >
                                        <Pie
                                            isAnimationActive={false}
                                            data={pieData}
                                            cx="50%"
                                            cy="50%"
                                            innerRadius={40}
                                            outerRadius={70}
                                            paddingAngle={2}
                                            dataKey="value"
                                        >
                                            {pieData.map((entry, index) => (
                                                <Cell key={`cell-${index}`} fill={entry.color} />
                                            ))}
                                        </Pie>
                                        <Tooltip
                                            content={<CustomPieTooltip />}
                                            allowEscapeViewBox={{ x: true, y: true }}
                                            position={pieTooltipPosition}
                                            wrapperStyle={{ zIndex: 100 }}
                                        />
                                    </PieChart>
                                </ResponsiveContainer>
                            ) : (
                                <div className="h-full flex items-center justify-center text-gray-400">
                                    {loading ? t('common.loading', '加载中...') : t('token_stats.no_data', '暂无数据')}
                                </div>
                            )}
                        </div>
                        <div className="mt-4 space-y-2 max-h-32 overflow-y-auto">
                            {pieData.map((account) => (
                                <div key={account.fullEmail} className="flex items-center justify-between gap-3 text-sm">
                                    <div className="flex items-center gap-2">
                                        <div
                                            className="w-3 h-3 rounded-full"
                                            style={{ backgroundColor: account.color }}
                                        />
                                        <span className="text-gray-600 dark:text-gray-300 truncate max-w-[160px]" title={account.fullEmail}>
                                            {account.name}
                                        </span>
                                    </div>
                                    <span className="font-medium text-gray-800 dark:text-white">
                                        {formatNumber(account.value)}
                                    </span>
                                </div>
                            ))}
                        </div>
                    </div>
                </div>


                {
                    modelData.length > 0 && viewMode === 'model' && (
                        <div className="console-panel min-w-0">
                            <h2 className="text-lg font-semibold text-gray-800 dark:text-white mb-4 flex items-center gap-2">
                                <Cpu className="w-5 h-5 text-blue-500" />
                                {t('token_stats.model_details', '分模型详细统计')}
                            </h2>
                            <div className="overflow-x-auto">
                                <table className="w-full min-w-[800px] text-sm">
                                    <thead>
                                        <tr className="border-b border-gray-200 dark:border-gray-700">
                                            <th className="text-left py-3 px-4 font-medium text-gray-500 dark:text-gray-400">
                                                {t('token_stats.model', '模型')}
                                            </th>
                                            <th className="text-right py-3 px-4 font-medium text-gray-500 dark:text-gray-400">
                                                {t('token_stats.requests', '请求数')}
                                            </th>
                                            <th className="text-right py-3 px-4 font-medium text-gray-500 dark:text-gray-400">
                                                {t('token_stats.input', '输入')}
                                            </th>
                                            <th className="text-right py-3 px-4 font-medium text-gray-500 dark:text-gray-400">
                                                {t('token_stats.output', '输出')}
                                            </th>
                                            <th className="text-right py-3 px-4 font-medium text-gray-500 dark:text-gray-400">
                                                {t('token_stats.cached_token', '缓存命中')}
                                            </th>
                                            <th className="text-right py-3 px-4 font-medium text-gray-500 dark:text-gray-400">
                                                {t('token_stats.total', '合计')}
                                            </th>
                                            <th className="text-right py-3 px-4 font-medium text-gray-500 dark:text-gray-400">
                                                {t('token_stats.percentage', '占比')}
                                            </th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {modelData.map((model, index) => {
                                            const percentage = summary && summary.total_tokens > 0 ? ((model.total_tokens / summary.total_tokens) * 100).toFixed(1) : '0';
                                            return (
                                                <tr
                                                    key={model.model}
                                                    className="border-b border-gray-100 dark:border-gray-700/50 hover:bg-gray-50 dark:hover:bg-gray-700/30"
                                                >
                                                    <td className="py-3 px-4">
                                                        <div className="flex items-center gap-2">
                                                            <div
                                                                className="w-3 h-3 rounded-full"
                                                                style={{ backgroundColor: MODEL_COLORS[index % MODEL_COLORS.length] }}
                                                            />
                                                            <span className="text-gray-800 dark:text-white font-medium">
                                                                {model.model}
                                                            </span>
                                                        </div>
                                                    </td>
                                                    <td className="py-3 px-4 text-right text-gray-600 dark:text-gray-300">
                                                        {model.request_count.toLocaleString()}
                                                    </td>
                                                    <td className="py-3 px-4 text-right text-blue-600">
                                                        {formatNumber(model.total_input_tokens)}
                                                    </td>
                                                    <td className="py-3 px-4 text-right text-purple-600">
                                                        {formatNumber(model.total_output_tokens)}
                                                    </td>
                                                    <td className="py-3 px-4 text-right text-sky-600">
                                                        {formatNumber(model.total_cached_tokens)}
                                                    </td>
                                                    <td className="py-3 px-4 text-right font-semibold text-gray-800 dark:text-white">
                                                        {formatNumber(model.total_tokens)}
                                                    </td>
                                                    <td className="py-3 px-4 text-right">
                                                        <div className="flex items-center justify-end gap-2">
                                                            <div className="w-16 bg-gray-200 dark:bg-gray-700 rounded-full h-2">
                                                                <div
                                                                    className="h-2 rounded-full"
                                                                    style={{
                                                                        width: `${percentage}%`,
                                                                        backgroundColor: MODEL_COLORS[index % MODEL_COLORS.length]
                                                                    }}
                                                                />
                                                            </div>
                                                            <span className="text-gray-600 dark:text-gray-300 w-12 text-right">
                                                                {percentage}%
                                                            </span>
                                                        </div>
                                                    </td>
                                                </tr>
                                            );
                                        })}
                                    </tbody>
                                </table>
                            </div>
                        </div>
                    )
                }



                {
                    accountData.length > 0 && viewMode === 'account' && (
                        <div className="console-panel min-w-0">
                            <h2 className="text-lg font-semibold text-gray-800 dark:text-white mb-4">
                                {t('token_stats.account_details', '账号详细统计')}
                            </h2>
                            <div className="overflow-x-auto">
                                <table className="w-full min-w-[720px] text-sm">
                                    <thead>
                                        <tr className="border-b border-gray-200 dark:border-gray-700">
                                            <th className="text-left py-3 px-4 font-medium text-gray-500 dark:text-gray-400">
                                                {t('token_stats.account', '账号')}
                                            </th>
                                            <th className="text-right py-3 px-4 font-medium text-gray-500 dark:text-gray-400">
                                                {t('token_stats.requests', '请求数')}
                                            </th>
                                            <th className="text-right py-3 px-4 font-medium text-gray-500 dark:text-gray-400">
                                                {t('token_stats.input', '输入')}
                                            </th>
                                            <th className="text-right py-3 px-4 font-medium text-gray-500 dark:text-gray-400">
                                                {t('token_stats.output', '输出')}
                                            </th>
                                            <th className="text-right py-3 px-4 font-medium text-gray-500 dark:text-gray-400">
                                                {t('token_stats.cached_token', '缓存命中')}
                                            </th>
                                            <th className="text-right py-3 px-4 font-medium text-gray-500 dark:text-gray-400">
                                                {t('token_stats.total', '合计')}
                                            </th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {accountData.map((account) => (
                                            <tr
                                                key={account.account_email}
                                                className="border-b border-gray-100 dark:border-gray-700/50 hover:bg-gray-50 dark:hover:bg-gray-700/30"
                                            >
                                                <td className="py-3 px-4 text-gray-800 dark:text-white">
                                                    {account.account_email}
                                                </td>
                                                <td className="py-3 px-4 text-right text-gray-600 dark:text-gray-300">
                                                    {account.request_count.toLocaleString()}
                                                </td>
                                                <td className="py-3 px-4 text-right text-blue-600">
                                                    {formatNumber(account.total_input_tokens)}
                                                </td>
                                                <td className="py-3 px-4 text-right text-purple-600">
                                                    {formatNumber(account.total_output_tokens)}
                                                </td>
                                                <td className="py-3 px-4 text-right text-sky-600">
                                                    {formatNumber(account.total_cached_tokens)}
                                                </td>
                                                <td className="py-3 px-4 text-right font-semibold text-gray-800 dark:text-white">
                                                    {formatNumber(account.total_tokens)}
                                                </td>
                                            </tr>
                                        ))}
                                    </tbody>
                                </table>
                            </div>
                        </div>
                    )
                }
            </div>
        </div>
    );
};

export default TokenStats;
