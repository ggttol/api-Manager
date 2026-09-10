import React from 'react';
import { ProxyMonitor } from '../components/proxy/ProxyMonitor';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/common/ConsolePage';

const Monitor: React.FC = () => {
    const { t, i18n } = useTranslation();
    return (
        <div className="console-page console-page-fixed overflow-y-auto">
            <PageHeader
                title={t('console.monitor_title', { defaultValue: i18n.language.startsWith('zh') ? '请求日志' : 'Request logs' })}
                description={t('console.monitor_description', { defaultValue: i18n.language.startsWith('zh') ? '检查请求状态、用量与响应详情。' : 'Inspect request status, token usage and response details.' })}
            />
            <ProxyMonitor className="flex-1 min-h-[420px]" />
        </div>
    );
};

export default Monitor;