import { useEffect, type ReactElement, type ReactNode } from 'react';
import { useQuery } from '@tanstack/react-query';
import { api } from '../api';
import { useI18n } from '../i18n';
import './CwdField.css';

interface Props {
  value: string;
  onChange: (v: string) => void;
  /** 字段标签（StartDialog=工程 (cwd)；定时任务表单=工作目录 *） */
  label: ReactNode;
  /** 供 getByLabelText 的输入框 id */
  inputId?: string;
}

/// cwd 选择器（用户新工单，新建会话/定时任务共用）：
/// 工程树选项（主仓 optgroup + worktree 子项，分支小字）仅作**回填源**——
/// 输入框是唯一事实值，恒可手输服务机上的新绝对路径（树外目录不受选项限制）。
export default function CwdField({ value, onChange, label, inputId }: Props): ReactElement {
  const t = useI18n();
  const tree = useQuery({
    queryKey: ['projectTree'],
    queryFn: api.projectTree,
    staleTime: 15_000,
  });
  const groups = tree.data ?? [];

  // 空值一次性回填：树就绪且未选 → 取首个主仓（保留「打开即有默认 cwd」的既有体验）
  useEffect(() => {
    if (value === '' && groups.length > 0) onChange(groups[0].main.path);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 仅空值回填一次；value/onChange 不入依赖防回填后反复触发
  }, [groups.length]);

  return (
    <div className="cwd-field">
      <label className="cwd-field-label" htmlFor={inputId}>
        {label}
      </label>
      <input
        id={inputId}
        className="cwd-field-input"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder="/absolute/path"
        spellCheck={false}
      />
      {groups.length > 0 ? (
        <select
          className="cwd-field-pick"
          aria-label={t('cwd.pick')}
          value=""
          onChange={(e) => {
            if (e.target.value !== '') onChange(e.target.value); // 选中即回填输入框
          }}
        >
          <option value="">{t('cwd.pickPh')}</option>
          {groups.map((g) => (
            <optgroup
              key={g.main.path}
              label={`${g.main.name}${g.main.branch ? ` · ${g.main.branch}` : ''}`}
            >
              <option value={g.main.path}>
                {g.main.name}
                {g.main.branch ? ` · ${g.main.branch}` : ''}
              </option>
              {g.worktrees.map((w) => (
                <option key={w.path} value={w.path}>
                  {'↳ '}
                  {w.name}
                  {w.branch ? ` · ${w.branch}` : ''}
                </option>
              ))}
            </optgroup>
          ))}
        </select>
      ) : null}
      <p className="cwd-field-hint">{t('cwd.serverPathHint')}</p>
    </div>
  );
}
