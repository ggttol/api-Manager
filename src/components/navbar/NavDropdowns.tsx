import { Languages } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import type { Language } from './constants';

interface LanguageDropdownProps {
    currentLanguage: string;
    languages: Language[];
    onLanguageChange: (langCode: string) => void;
}

export function LanguageDropdown({ currentLanguage, languages, onLanguageChange }: LanguageDropdownProps) {
    const { t } = useTranslation();
    return (
        <div className="console-language-control">
            <Languages size={17} aria-hidden="true" />
            <select
                aria-label={t('settings.general.language')}
                title={t('settings.general.language')}
                value={currentLanguage}
                onChange={(event) => onLanguageChange(event.target.value)}
            >
                {languages.map(language => (
                    <option key={language.code} value={language.code}>{language.label}</option>
                ))}
            </select>
        </div>
    );
}
