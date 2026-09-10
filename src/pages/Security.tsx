import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Shield, Lock, FileText, Settings, Activity, RefreshCw } from 'lucide-react';
import { IpAccessLogs } from '../components/security/IpAccessLogs';
import { BlacklistManager } from '../components/security/BlacklistManager';
import { WhitelistManager } from '../components/security/WhitelistManager';
import { SecurityConfig } from '../components/security/SecurityConfig';
import { IpStatistics } from '../components/security/IpStatistics';
import { PageHeader } from '../components/common/ConsolePage';

const Security: React.FC = () => {
    const { t, i18n } = useTranslation();
    const [activeTab, setActiveTab] = useState<'logs' | 'stats' | 'blacklist' | 'whitelist' | 'config'>('logs');
    const [refreshKey, setRefreshKey] = useState(0);

    const handleRefresh = () => {
        setRefreshKey(prev => prev + 1);
    };

    const renderContent = () => {
        switch (activeTab) {
            case 'logs':
                return <IpAccessLogs refreshKey={refreshKey} />;
            case 'stats':
                return <IpStatistics refreshKey={refreshKey} />;
            case 'blacklist':
                return <BlacklistManager refreshKey={refreshKey} />;
            case 'whitelist':
                return <WhitelistManager refreshKey={refreshKey} />;
            case 'config':
                return <SecurityConfig />;
            default:
                return <IpAccessLogs refreshKey={refreshKey} />;
        }
    };

    const tabs = [
        { id: 'logs', label: t('security.tab_logs'), icon: FileText },
        { id: 'stats', label: t('security.tab_stats'), icon: Activity },
        { id: 'blacklist', label: t('security.tab_blacklist'), icon: Shield },
        { id: 'whitelist', label: t('security.tab_whitelist'), icon: Lock },
        { id: 'config', label: t('security.tab_config'), icon: Settings },
    ];

    return (
        <div className="console-page console-page-fixed overflow-y-auto">
            <PageHeader
                title={t('nav.security')}
                description={t('console.security_description', { defaultValue: i18n.language.startsWith('zh') ? '审查访问记录并管理 IP 安全策略。' : 'Review access activity and manage IP security policies.' })}
                actions={activeTab !== 'config' && <button onClick={handleRefresh} className="console-button" title={t('security.refresh_data')}>
                    <RefreshCw size={16} />{t('security.refresh')}
                </button>}
            />

            <div className="min-w-0 overflow-x-auto shrink-0">
                <div className="console-tabs w-max min-w-full" role="tablist" aria-label={t('nav.security')}>
                    {tabs.map((tab) => (
                        <button
                            key={tab.id}
                            role="tab"
                            aria-selected={activeTab === tab.id}
                            onClick={() => setActiveTab(tab.id as typeof activeTab)}
                            className={`console-tab whitespace-nowrap ${activeTab === tab.id ? 'active' : ''}`}
                        >
                            <tab.icon size={18} />
                            {tab.label}
                        </button>
                    ))}
                </div>
            </div>

            <div className="console-panel !p-0 flex-1 min-h-[420px] min-w-0 overflow-hidden flex flex-col">
                {renderContent()}
            </div>
        </div>
    );
};

export default Security;
