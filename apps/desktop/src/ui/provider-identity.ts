import projection from '../../../../assets/model-data/current/runtime-projection.json';

// Deterministic projection of the same source as the daemon's bundled catalog.
const templates = projection.connection_templates;
export function connectionTemplate(optionId?: string | null) {
  return templates.find(template => template.connection_option_id === optionId);
}
export function connectionName(optionId: string | null | undefined, language: 'zh' | 'en', fallback = '') {
  const template = connectionTemplate(optionId);
  return template?.name[language] || template?.name[language === 'zh' ? 'en' : 'zh'] || fallback;
}
