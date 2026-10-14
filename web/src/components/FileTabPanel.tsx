import { type ReactElement } from 'react';
import FileContentView from './FileContentView';
import './FileTabPanel.css';

interface Props {
  /** 文件绝对路径（白名单内） */
  path: string;
  /** 是否当前聚焦页签；非聚焦仅 CSS 隐藏不卸载（滚动/加载状态保留） */
  active: boolean;
}

/// 文件页签面板（agent-hub-files-panel F2 v2 主区 Tab 化）：每个打开的文件一个
/// 常驻面板，切走仅 display:none（多文件并存互不冲掉、滚动位置保留），关闭
/// （卸载）由页签条负责（App 持有 fileTabs 状态）。
export default function FileTabPanel({ path, active }: Props): ReactElement {
  return (
    <div className={`file-tab-panel${active ? '' : ' file-tab-panel--hidden'}`}>
      <div className="file-tab-path" title={path}>
        {path}
      </div>
      <div className="file-tab-body">
        <FileContentView path={path} />
      </div>
    </div>
  );
}
