/// 测试环境 i18n 固定策略（agent-hub-settings C1）：默认固定 zh locale——
/// 现有测试的中文断言（key 对应中文原文）无需全量改。需要测英文态的用例
/// 自行 setLangChoice('en') 并在用例尾部恢复（或用 _withLocale 辅助）。
import { setLangChoice } from './i18n';

setLangChoice('zh');
