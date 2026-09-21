import { invoke } from '@tauri-apps/api/core';
import { copyText, Dialog, UiIcon } from '../ui';
import { useState } from 'react';

export const CLASSIFIER_OPENAPI_FILENAME = 'hiroute-decision.openapi.json';

export const classifierCurlExample = `curl --request POST 'https://classifier.example/v1/decisions' \\
  --header 'Content-Type: application/json' \\
  --data-raw '{
    "branches": {
      "smart_saving_simple": "Use the economy model group for a clear, well-scoped task.",
      "smart_saving_complex": "Use the primary model group for an ambiguous, cross-module, diagnostic, concurrent, or deep-reasoning task."
    },
    "latest_user": [{"kind":"text","text":"Fix this failing test."}],
    "visible_conversation": [],
    "history_partial": false,
    "assessment_from": null
  }'`;

const classifierResponseExample = `{
  "branch_id": "smart_saving_complex",
  "assessment": {
    "score": 0.82,
    "partial": false
  }
}`;

export async function saveClassifierOpenApi(): Promise<'saved' | 'cancelled'> {
  return invoke<'saved' | 'cancelled'>('save_classifier_openapi');
}

export function ClassifierProtocolDialog({
  open,
  language,
  onClose,
}: {
  open: boolean;
  language: 'zh' | 'en';
  onClose: () => void;
}) {
  const en = language === 'en';
  const text = (zh: string, english: string) => en ? english : zh;
  const [actionStatus, setActionStatus] = useState<'idle' | 'copied' | 'copy_failed' | 'saving' | 'saved' | 'save_cancelled' | 'save_failed'>('idle');
  const close = () => {
    setActionStatus('idle');
    onClose();
  };

  return <Dialog
    open={open}
    title={text('自定义分类服务接入协议', 'Custom classifier service protocol')}
    description={text('HiRoute 向你配置的完整地址发送一次 HTTP POST JSON 请求。', 'HiRoute sends one HTTP POST JSON request to the complete endpoint you configure.')}
    closeLabel={text('关闭接入协议', 'Close protocol')}
    onClose={close}
    footer={<button className="btn btn-primary" type="button" onClick={close}>{text('完成', 'Done')}</button>}
  >
    <div className="classifier-protocol">
      <div className="callout"><UiIcon name="info" /><span>{text('服务必须从请求允许的分支中选择一个；有可评分的上一阶段时，还可以返回可选胜任度评分。服务可基于 Jev、LLM 或其他自定义策略实现。', 'The service must choose one allowed request branch. When a preceding stage is assessable, it may also return an optional competence assessment. The service can use Jev, an LLM, or another custom strategy.')}</span></div>
      <section>
        <h3>{text('请求示例', 'Request example')}</h3>
        <p>{text('将示例地址替换为你的服务地址；如已配置认证，HiRoute 会额外发送对应请求头。', 'Replace the example URL with your service endpoint. When authentication is configured, HiRoute also sends that header.')}</p>
        <pre><code>{classifierCurlExample}</code></pre>
        <div className="classifier-protocol-actions">
          <button className="btn" type="button" onClick={() => {
            void copyText(classifierCurlExample)
              .then(() => setActionStatus('copied'))
              .catch(() => setActionStatus('copy_failed'));
          }}><UiIcon name="copy" />{text('复制 curl', 'Copy curl')}</button>
          <button className="btn" type="button" disabled={actionStatus === 'saving'} onClick={() => {
            setActionStatus('saving');
            void saveClassifierOpenApi()
              .then(outcome => setActionStatus(outcome === 'saved' ? 'saved' : 'save_cancelled'))
              .catch(() => setActionStatus('save_failed'));
          }}><UiIcon name="download" />{text('保存 OpenAPI', 'Save OpenAPI')}</button>
          <span className="field-help" role="status" aria-live="polite">{
            actionStatus === 'copied' ? text('已复制', 'Copied')
              : actionStatus === 'copy_failed' ? text('复制失败', 'Copy failed')
                : actionStatus === 'saving' ? text('请选择保存位置', 'Choose where to save the file')
                  : actionStatus === 'saved' ? text(`已保存：${CLASSIFIER_OPENAPI_FILENAME}`, `Saved: ${CLASSIFIER_OPENAPI_FILENAME}`)
                    : actionStatus === 'save_cancelled' ? text('已取消保存', 'Save cancelled')
                      : actionStatus === 'save_failed' ? text('保存失败，请重试。', 'Save failed. Try again.')
                        : ''
          }</span>
        </div>
      </section>
      <section>
        <h3>{text('响应示例', 'Response example')}</h3>
        <pre><code>{classifierResponseExample}</code></pre>
        <p>{text('branch_id 必须来自本次 branches。assessment 仅在 assessment_from 不为 null 时有效；reason 可选。', 'branch_id must come from this request\'s branches. assessment is valid only when assessment_from is non-null; reason is optional.')}</p>
      </section>
    </div>
  </Dialog>;
}
