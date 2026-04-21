import './OperationSteps.css';

interface ToolStep {
  id: string;
  name: string;
  input: any;
  result?: string;
  isError?: boolean;
  timestamp: number;
  status: 'running' | 'completed' | 'error';
}

interface Props {
  steps: ToolStep[];
}

export default function OperationSteps({ steps }: Props) {
  if (steps.length === 0) return null;

  return (
    <div className="operation-steps">
      <div className="steps-header">Operation Steps</div>
      <div className="steps-timeline">
        {steps.map((step, idx) => (
          <div key={step.id} className={`step-item step-${step.status}`}>
            <div className="step-indicator">
              <div className="step-dot" />
              {idx < steps.length - 1 && <div className="step-line" />}
            </div>
            <div className="step-content">
              <div className="step-header">
                <span className="step-name">{step.name}</span>
                <span className={`step-badge ${step.status}`}>
                  {step.status === 'running' ? 'Running...' : step.status === 'error' ? 'Failed' : 'Done'}
                </span>
              </div>
              {step.input && (
                <div className="step-input">
                  {typeof step.input === 'object'
                    ? JSON.stringify(step.input, null, 2).substring(0, 200)
                    : String(step.input).substring(0, 200)}
                </div>
              )}
              {step.result && (
                <div className={`step-result ${step.isError ? 'error' : ''}`}>
                  {step.result.substring(0, 300)}
                </div>
              )}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
