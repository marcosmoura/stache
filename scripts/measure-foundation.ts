import { helpText, parseCommandLine, UsageError } from './measure/cli';
import { InterruptedError, runMeasurement } from './measure/run';

async function main(): Promise<number> {
  let parsed: ReturnType<typeof parseCommandLine>;
  try {
    parsed = parseCommandLine(process.argv.slice(2));
  } catch (error) {
    if (error instanceof UsageError) {
      process.stderr.write(`measure: ${error.message}\n\n${helpText()}`);
      return 2;
    }
    throw error;
  }

  if (parsed.kind === 'help') {
    process.stdout.write(helpText());
    return 0;
  }

  try {
    const outcome = await runMeasurement(parsed.options);
    process.stdout.write(
      `measure: report ${outcome.outputs.reportPath}\n` +
        `measure: samples ${outcome.outputs.samplesPath}\n`,
    );
    const cleanup = outcome.clean
      ? 'cleanup complete: temporary roots removed and default audio devices verified'
      : 'cleanup incomplete: see the safety record in the report';
    if (outcome.failure === null) {
      process.stdout.write(`measure: completed; ${cleanup}\n`);
      return outcome.clean ? 0 : 1;
    }
    process.stderr.write(
      `measure: ${outcome.interrupted ? 'interrupted' : 'failed'}: ${outcome.failure.message}\n` +
        `measure: ${cleanup}\n`,
    );
    return outcome.interrupted ? 130 : 1;
  } catch (error) {
    if (error instanceof InterruptedError) {
      process.stderr.write(
        `measure: interrupted before a report could be written: ${error.message}\n`,
      );
      return 130;
    }
    process.stderr.write(`measure: ${error instanceof Error ? error.message : String(error)}\n`);
    return 1;
  }
}

try {
  process.exitCode = await main();
} catch (error) {
  process.stderr.write(`measure: ${error instanceof Error ? error.message : String(error)}\n`);
  process.exitCode = 1;
}
