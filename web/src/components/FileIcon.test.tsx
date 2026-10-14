// @vitest-environment happy-dom
// FileIcon：extFor 映射（常见扩展/大小写/点开头/License 特判/未知兜底）+ 渲染 data-icon。
import { cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import FileIcon, { extFor } from './FileIcon';

afterEach(cleanup);

describe('extFor（文件名 → 图标键）', () => {
  it('常见扩展名映射（大小写不敏感）', () => {
    expect(extFor('README.md')).toBe('md');
    expect(extFor('app.TS')).toBe('ts');
    expect(extFor('main.rs')).toBe('rs');
    expect(extFor('server.go')).toBe('go');
    expect(extFor('Cargo.toml')).toBe('toml');
    expect(extFor('index.html')).toBe('html');
    expect(extFor('photo.JPEG')).toBe('jpeg');
    expect(extFor('Cargo.lock')).toBe('lock');
    expect(extFor('run.sh')).toBe('sh');
  });

  it('点开头文件：.env/.gitignore 取点名', () => {
    expect(extFor('.env')).toBe('env');
    expect(extFor('.gitignore')).toBe('gitignore');
  });

  it('LICENSE 全名特判；LICENSE.md 归 md（扩展名优先）', () => {
    expect(extFor('LICENSE')).toBe('license');
    expect(extFor('LICENSE.md')).toBe('md');
  });

  it('无扩展名/未知类型 → file 兜底；嵌套路径取末段', () => {
    expect(extFor('Makefile')).toBe('file');
    expect(extFor('unknown.xyz')).toBe('file');
    expect(extFor('deep/nested/README.md')).toBe('md');
  });
});

describe('FileIcon 渲染', () => {
  it('data-icon 随映射；未知走 file 兜底（白描页形）', () => {
    const { container, rerender } = render(<FileIcon name="README.md" />);
    expect(container.querySelector('svg')?.getAttribute('data-icon')).toBe('md');
    rerender(<FileIcon name="unknown.xyz" />);
    expect(container.querySelector('svg')?.getAttribute('data-icon')).toBe('file');
  });
});
