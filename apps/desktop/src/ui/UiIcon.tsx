import { prototypeIcons } from '../occami/icons';
export type UiIconName =
  | 'home'
  | 'models'
  | 'route'
  | 'agent'
  | 'sessions'
  | 'tasks'
  | 'settings'
  | 'sparkles'
  | 'plus'
  | 'plug'
  | 'key'
  | 'scan'
  | 'arrow'
  | 'arrowLeft'
  | 'check'
  | 'warning'
  | 'clock'
  | 'activity'
  | 'chevron'
  | 'chevronRight'
  | 'copy'
  | 'download'
  | 'trash'
  | 'refresh'
  | 'search'
  | 'info'
  | 'lock'
  | 'grip'
  | 'arrowUp'
  | 'arrowDown'
  | 'upload'
  | 'close';

const aliases: Partial<Record<UiIconName, keyof typeof prototypeIcons>> = {arrow:'arrowRight', warning:'alert', chevron:'chevronDown'};
export function UiIcon({ name, className = '' }: { name: UiIconName; className?: string }) {
  const key = aliases[name] ?? name;
  const body = prototypeIcons[key as keyof typeof prototypeIcons] ?? prototypeIcons.info;
  return <svg className={`icon ${className}`.trim()} viewBox="0 0 24 24" aria-hidden="true" dangerouslySetInnerHTML={{__html:body}} />;
}
