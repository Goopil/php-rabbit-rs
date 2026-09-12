<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Goopil\RabbitRs\Laravel\Exceptions\SupervisorException;
use Symfony\Component\Process\Process;

/**
 * @phpstan-type ProcessFactory \Closure(int): Process
 * @phpstan-type WorkPlanEntry array{connection: string, queues: list<string>}
 * @phpstan-type WorkerOptions array{timeout?: int|null, tries?: int|null, memory?: int|null, max-jobs?: int|null, max-time?: int|null, stop-when-empty?: bool}
 * @phpstan-type DepthSample array<string, int|null>
 * @phpstan-type ChildSlot array{process: Process, entry: int, restarts: int, restartAt: float, stopping: bool, stoppingAt: float}
 */
class WorkerSupervisor
{
    public const EXIT_CLEAN = 0;

    public const EXIT_MAX_RESTARTS = 1;

    /**
     * Environment variable used to pass the worker index to child processes.
     * Deliberately distinct from RABBIT_RS_WORKER, which is the worker MODE
     * (default|horizon) read from config/rabbit-rs.php: overriding it here
     * would silently downgrade Horizon users' supervised children.
     */
    public const WORKER_ENV = 'RABBIT_RS_WORKER_INDEX';

    /**
     * Worker options that are propagated to each child `queue:work` process.
     * Null values are omitted from the child command.
     */
    private const PROPAGATED_OPTIONS = ['timeout', 'tries', 'memory', 'max-jobs', 'max-time'];

    /**
     * How many times the one-shot final depth check may re-arm the initial
     * fleet when it still finds work: bounds the pathological loop where
     * late-async-flushed messages keep the fleet spinning.
     */
    private const MAX_ONE_SHOT_REARMS = 3;

    /**
     * Seconds between a downscale SIGTERM and the SIGKILL escalation for a
     * child that has not exited yet.
     */
    private const STOP_ESCALATION_SECONDS = 15.0;

    private readonly int $initialWorkers;

    private readonly ?WorkScalePolicy $scalePolicy;

    /**
     * Monotonic allocator for worker indexes: dynamically spawned children
     * must never collide on --name=worker-{i} or RABBIT_RS_WORKER_INDEX,
     * including across re-arms and scaling shifts.
     */
    private int $nextWorkerIndex = 0;

    /**
     * @param  list<WorkPlanEntry>  $plan  One entry per targeted connection; each
     *                                     child consumes one entry's queues via
     *                                     `queue:work <connection> --queue=<q1,q2>` (the connection is
     *                                     the positional argument of Laravel's WorkCommand).
     * @param  int  $workers  Children spawned per plan entry (the initial fleet
     *                        when auto-scaling is configured).
     * @param  ?ProcessFactory  $processFactory  Optional override used by tests
     *                                           to spawn a stub process instead of `queue:work`.
     * @param  WorkerOptions  $options  Worker options to propagate to child processes.
     *                                  Keys: timeout, tries, memory, max-jobs, max-time. Null values are omitted.
     * @param  ?int  $minWorkers  Auto-scaling floor per connection (downscaling
     *                            never goes below it; null means 1).
     * @param  ?int  $maxWorkers  Auto-scaling ceiling per connection; null (the
     *                            default) keeps the fixed `--workers` fleet and disables scaling.
     * @param  float  $scaleCooldownSeconds  Minimum seconds between two scaling
     *                                       passes (sampling included).
     * @param  int  $scaleIdleSeconds  Seconds of continuous empty depth before a
     *                                 long-running connection releases idle workers.
     * @param  bool  $stopWhenEmpty  Once mode: children receive
     *                               `--stop-when-empty` and the supervisor exits once every child has
     *                               terminated (also honored through $options for backwards compatibility).
     * @param  bool  $once  Once mode: children receive `--once` (a single job
     *                      each) under the same supervision semantics.
     * @param  (\Closure(): DepthSample)|null  $depthCallback  Samples the ready
     *                                                         depth per connection name (null when unknown). Injected so tests
     *                                                         can fake it without HTTP; when absent, scaling and the one-shot
     *                                                         final depth check never fire.
     */
    public function __construct(
        private readonly array $plan,
        private readonly int $workers,
        private readonly int $maxRestarts,
        private readonly int $baseBackoffSeconds,
        private readonly ?\Closure $processFactory = null,
        private readonly array $options = [],
        private readonly ?int $minWorkers = null,
        private readonly ?int $maxWorkers = null,
        private readonly float $scaleCooldownSeconds = 3.0,
        private readonly int $scaleIdleSeconds = 30,
        private readonly bool $stopWhenEmpty = false,
        private readonly bool $once = false,
        private readonly ?\Closure $depthCallback = null,
    ) {
        $this->initialWorkers = $this->maxWorkers !== null
            ? min($this->workers, $this->maxWorkers)
            : $this->workers;
        $this->scalePolicy = $this->maxWorkers !== null
            ? new WorkScalePolicy($this->minWorkers ?? 1, $this->maxWorkers, $this->scaleCooldownSeconds, $this->scaleIdleSeconds)
            : null;
    }

    /**
     * Build one child command per plan entry × worker, with worker indexes
     * numbered across the full child list.
     *
     * The worker index is passed via the RABBIT_RS_WORKER_INDEX environment
     * variable (see {@see workerEnvironment()}) rather than as a CLI option,
     * because `queue:work` is Laravel's built-in command and Symfony Console
     * rejects unknown options. The `--name` option (recognised by `queue:work`) is
     * set to a unique value so the worker name appears in logs and metrics.
     *
     * Worker options (timeout, tries, memory, max-jobs, max-time) are
     * propagated when set; null-valued options are omitted.
     *
     * @return list<list<string>>
     */
    public function buildChildCommands(): array
    {
        $commands = [];
        $index = 0;
        foreach ($this->plan as $entry) {
            for ($worker = 0; $worker < $this->initialWorkers; $worker++) {
                $commands[] = $this->childCommand($index, $entry);
                $index++;
            }
        }

        return $commands;
    }

    /**
     * @param  WorkPlanEntry  $entry
     * @return list<string>
     */
    private function childCommand(int $workerIndex, array $entry): array
    {
        // The connection is `queue:work`'s positional argument (Laravel's
        // WorkCommand signature is `queue:work {connection?}`): passing it as
        // an option would be rejected by Symfony Console.
        $cmd = [
            PHP_BINARY,
            'artisan',
            'queue:work',
            $entry['connection'],
            '--queue='.implode(',', $entry['queues']),
            '--name=worker-'.$workerIndex,
        ];

        foreach (self::PROPAGATED_OPTIONS as $opt) {
            $value = $this->options[$opt] ?? null;
            if ($value !== null) {
                $cmd[] = "--{$opt}={$value}";
            }
        }

        if ($this->once) {
            $cmd[] = '--once';
        }

        if ($this->stopsWhenEmpty()) {
            $cmd[] = '--stop-when-empty';
        }

        return $cmd;
    }

    /**
     * Returns the environment variable name used to pass the worker index.
     */
    public static function workerEnv(): string
    {
        return self::WORKER_ENV;
    }

    /**
     * Whether the once mode (one-shot supervision) is active: children run
     * once (`--once` and/or `--stop-when-empty` propagated to the child) and
     * are never recycled or restarted; the supervisor exits once every child
     * has terminated, propagating the highest child exit status.
     */
    private function isOneShot(): bool
    {
        return $this->once || $this->stopsWhenEmpty();
    }

    private function stopsWhenEmpty(): bool
    {
        return $this->stopWhenEmpty || (bool) ($this->options['stop-when-empty'] ?? false);
    }

    /**
     * Returns the environment variables to set when spawning the given worker.
     *
     * @return array<string, string>
     */
    public function workerEnvironment(int $workerIndex): array
    {
        return [self::WORKER_ENV => (string) $workerIndex];
    }

    public function shouldRestart(int $currentRestarts): bool
    {
        return $currentRestarts < $this->maxRestarts;
    }

    public function backoffSeconds(int $currentRestarts): int
    {
        $seconds = $this->baseBackoffSeconds * (2 ** $currentRestarts);

        return min($seconds, 60);
    }

    /**
     * Starts the supervisor loop. Each child runs queue:work with its plan
     * entry's connection and queues. On signal SIGTERM/SIGINT, children are
     * stopped gracefully. A clean child exit (exit code 0, e.g. --max-jobs
     * recycling) restarts the child immediately and resets its crash budget;
     * a non-zero exit is a crash: the child is restarted with backoff until
     * maxRestarts is reached.
     *
     * Once mode ({@see isOneShot()}): exited children are never recycled; the
     * supervisor exits when the fleet drains, after a final depth check that
     * may re-arm the initial fleet when the management API still reports work.
     *
     * Auto-scaling (a maxWorkers bound plus an injected depth callback):
     * admission scales the fleet up per connection while the depth outpaces
     * the live workers, respecting the policy's cooldown and shift bounds.
     *
     * When ext-pcntl is not available and a single child is configured, the
     * child runs in the foreground without forking ({@see runInline()}); no
     * pcntl function is needed on that path.
     *
     * @throws SupervisorException when ext-pcntl is not available and more
     *                             than one child is configured
     */
    public function run(): int
    {
        if (! $this->canFork()) {
            $children = $this->buildChildCommands();
            if (count($children) === 1) {
                return $this->runInline($children);
            }

            throw new SupervisorException('ext-pcntl is required to supervise multiple workers. Install ext-pcntl or target a single connection.');
        }

        return $this->isOneShot() ? $this->runOneShot() : $this->runSupervised();
    }

    /**
     * Whether ext-pcntl is available for forking child processes.
     *
     * Overridden by test subclasses to simulate the absence of pcntl.
     */
    protected function canFork(): bool
    {
        return function_exists('pcntl_fork');
    }

    /**
     * Run a single child in the foreground without forking.
     *
     * Fallback for PHP builds without ext-pcntl (e.g. Windows): the child
     * process runs inline and the supervisor blocks until it exits, keeping
     * the same backoff and max-restarts semantics as the forking path.
     * Without pcntl there is no graceful signal handling: the default signal
     * disposition terminates the supervisor, leaving the child to stop on
     * its own.
     *
     * @param  list<list<string>>  $children  Exactly one child command.
     */
    private function runInline(array $children): int
    {
        $restarts = 0;
        $process = $this->startProcess(0, $children[0]);

        while (true) {
            $process->wait();

            if ($this->isOneShot()) {
                // Once mode: the child's exit is terminal, its status is the
                // supervisor's.
                return $process->getExitCode() ?? self::EXIT_CLEAN;
            }

            if ($this->isCleanExit($process)) {
                // Planned recycling (e.g. --max-jobs reached): reset the
                // crash budget and restart immediately, without backoff.
                $restarts = 0;
                $process = $this->startProcess(0, $children[0]);

                continue;
            }

            if (! $this->shouldRestart($restarts)) {
                return self::EXIT_MAX_RESTARTS;
            }

            sleep($this->backoffSeconds($restarts));
            $restarts++;
            $process = $this->startProcess(0, $children[0]);
        }
    }

    /**
     * Whether the child exited cleanly (planned recycling, e.g. --max-jobs
     * or --max-time reached): exit code 0. A clean exit resets the crash
     * budget and restarts immediately; only non-zero exits are treated as
     * crashes and consume the restart budget with backoff.
     */
    private function isCleanExit(Process $process): bool
    {
        return $process->getExitCode() === self::EXIT_CLEAN;
    }

    /**
     * Registers the SIGTERM/SIGINT handlers that flip the shutdown flag.
     *
     * @return \Closure(): bool the shared shutdown flag getter
     */
    private function installSignalHandlers(): \Closure
    {
        $shutdown = false;
        $signalHandler = static function () use (&$shutdown): void {
            $shutdown = true;
        };

        pcntl_async_signals(true);
        pcntl_signal(SIGTERM, $signalHandler);
        pcntl_signal(SIGINT, $signalHandler);

        return static function () use (&$shutdown): bool {
            return $shutdown;
        };
    }

    /**
     * Spawns the initial fleet: initialWorkers children per plan entry,
     * indexed by the monotonic allocator.
     *
     * @param  array<int, ChildSlot>  $slots
     */
    private function spawnInitialChildren(array &$slots): void
    {
        foreach (array_keys($this->plan) as $entryIndex) {
            for ($i = 0; $i < $this->initialWorkers; $i++) {
                $this->spawnSlot($slots, (int) $entryIndex);
            }
        }
    }

    /**
     * Spawns one child for a plan entry under a fresh, never-reused index.
     *
     * @param  array<int, ChildSlot>  $slots
     */
    private function spawnSlot(array &$slots, int $entryIndex): int
    {
        $index = $this->nextWorkerIndex++;
        $slots[$index] = [
            'process' => $this->startProcess($index, $this->childCommand($index, $this->plan[$entryIndex])),
            'entry' => $entryIndex,
            'restarts' => 0,
            'restartAt' => 0.0,
            'stopping' => false,
            'stoppingAt' => 0.0,
        ];

        return $index;
    }

    /**
     * @return array<int, ScaleState> one per plan entry, keyed by entry index
     */
    private function newScaleStates(): array
    {
        $states = [];
        foreach (array_keys($this->plan) as $entryIndex) {
            $states[(int) $entryIndex] = new ScaleState;
        }

        return $states;
    }

    /**
     * Whether auto-scaling is active: a max-workers bound and a depth source
     * are both required; anything less leaves the fleet static.
     */
    private function scalingEnabled(): bool
    {
        return $this->scalePolicy !== null && $this->depthCallback !== null;
    }

    /**
     * Whether the one-shot final depth check still finds work on any plan
     * connection (the late-async-flush guard): a null depth (no management
     * url, or a failed request) never counts as pending.
     */
    private function hasPendingWork(): bool
    {
        $depthCallback = $this->depthCallback;
        if ($depthCallback === null) {
            return false;
        }

        foreach ($depthCallback() as $depth) {
            if (is_int($depth) && $depth > 0) {
                return true;
            }
        }

        return false;
    }

    /**
     * Once mode: every child runs exactly once and is never restarted — a
     * clean exit removes its slot, a crash is remembered as the command's
     * exit status without touching the other children. When the fleet drains,
     * a final depth check re-arms the initial fleet while work remains on the
     * broker (bounded re-arms), otherwise the supervisor returns with the
     * highest child exit status. On SIGTERM/SIGINT, children are stopped
     * gracefully and the command exits clean.
     */
    private function runOneShot(): int
    {
        $slots = [];
        $this->spawnInitialChildren($slots);

        $isShutdown = $this->installSignalHandlers();

        $maxExit = null;
        $reArms = 0;
        $scaleStates = $this->newScaleStates();
        $lastScalePass = 0.0;

        while (! $isShutdown()) {
            $now = microtime(true);

            foreach (array_keys($slots) as $index) {
                $slot = $slots[$index];

                if ($slot['process']->isRunning()) {
                    continue;
                }

                // Once mode: every exit is terminal for its slot; a crashed
                // child's status fails the command instead of recycling.
                $exit = $slot['process']->getExitCode() ?? self::EXIT_CLEAN;
                $maxExit = $maxExit === null ? $exit : max($maxExit, $exit);
                unset($slots[$index]);
            }

            if ($slots === []) {
                if ($this->hasPendingWork() && $reArms < self::MAX_ONE_SHOT_REARMS) {
                    $reArms++;
                    $this->spawnInitialChildren($slots);

                    continue;
                }

                break;
            }

            if ($this->scalingEnabled() && $now - $lastScalePass >= $this->scaleCooldownSeconds) {
                $lastScalePass = $now;
                $this->runScalePass($now, $slots, $scaleStates, admitOnly: true);
            }

            usleep(100_000);
        }

        if ($isShutdown()) {
            $this->stopAllSlots($slots);

            return self::EXIT_CLEAN;
        }

        return $maxExit ?? self::EXIT_CLEAN;
    }

    /**
     * Long-running supervision: children are recycled on clean exit and
     * restarted with backoff on crash; the loop also runs the scaling passes
     * when auto-scaling is configured.
     */
    private function runSupervised(): int
    {
        $slots = [];
        $this->spawnInitialChildren($slots);

        $isShutdown = $this->installSignalHandlers();

        $scaleStates = $this->newScaleStates();
        $lastScalePass = 0.0;

        while (! $isShutdown()) {
            $now = microtime(true);

            foreach (array_keys($slots) as $index) {
                $slot = $slots[$index];

                if ($slot['process']->isRunning()) {
                    // A downscaled child that outlives its SIGTERM grace
                    // period gets SIGKILLed (it may be parked in a blocking
                    // native call that never wakes for the signal).
                    if ($slot['stopping'] && $now - $slot['stoppingAt'] > self::STOP_ESCALATION_SECONDS) {
                        $this->killSlot($slot['process']);
                    }

                    continue;
                }

                if ($slot['stopping']) {
                    // The released child exited: remove the slot without
                    // touching the crash budget — downscaling is not a crash.
                    unset($slots[$index]);

                    continue;
                }

                $exit = $this->superviseDeadSlot($index, $slots, $now);
                if ($exit !== null) {
                    return $exit;
                }
            }

            if ($this->scalingEnabled() && $now - $lastScalePass >= $this->scaleCooldownSeconds) {
                $lastScalePass = $now;
                $this->runScalePass($now, $slots, $scaleStates, admitOnly: false);
            }

            usleep(100_000);
        }

        $this->stopAllSlots($slots);

        return self::EXIT_CLEAN;
    }

    /**
     * One scaling pass: samples the per-connection depths through the
     * injected callback and applies the scale policy per plan entry. Depth
     * entries that are null (no management url, failed request) are skipped
     * silently, leaving that connection static.
     *
     * In one-shot mode the pass is admission-only: children self-terminate,
     * so no signals are ever sent. In long-running mode a Stop decision
     * releases the idlest children of the connection.
     *
     * @param  array<int, ChildSlot>  $slots
     * @param  array<int, ScaleState>  $scaleStates
     */
    private function runScalePass(float $now, array &$slots, array $scaleStates, bool $admitOnly): void
    {
        $depthCallback = $this->depthCallback;
        if ($depthCallback === null || $this->scalePolicy === null) {
            return;
        }

        $depths = $depthCallback();

        foreach (array_keys($this->plan) as $entryIndex) {
            $entry = $this->plan[$entryIndex];
            $depth = $depths[$entry['connection']] ?? null;
            if ($depth === null) {
                continue;
            }

            // One release at a time per connection: re-signaling a child
            // stuck in its graceful shutdown would keep pushing the SIGKILL
            // escalation forward, and walking the fleet below its floor
            // before the previous release lands is never wanted.
            if ($this->hasPendingRelease($slots, (int) $entryIndex)) {
                continue;
            }

            $action = $this->scalePolicy->decide(
                $now,
                (int) $depth,
                $this->liveCount($slots, (int) $entryIndex),
                $scaleStates[(int) $entryIndex],
            );

            for ($i = 0; $i < $action->up; $i++) {
                $this->spawnSlot($slots, (int) $entryIndex);
            }

            if (! $admitOnly && $action->down > 0) {
                $this->releaseIdleSlots($slots, (int) $entryIndex, $action->down, $now);
            }
        }
    }

    /**
     * Whether a downscaled child of one plan entry has not exited yet.
     *
     * @param  array<int, ChildSlot>  $slots
     */
    private function hasPendingRelease(array $slots, int $entryIndex): bool
    {
        foreach ($slots as $slot) {
            if ($slot['entry'] === $entryIndex && $slot['stopping']) {
                return true;
            }
        }

        return false;
    }

    /**
     * Releases the idlest children of one plan entry: the highest-index live
     * slots receive a NON-BLOCKING SIGTERM and their slot is marked stopping;
     * the poll loop observes the exit and removes the slot. Deliberately not
     * `Process::stop(10, ...)`, which would freeze the supervision loop for
     * up to 10 s per child — the loop must keep polling.
     *
     * Caveat (blocking native calls freeze signal handlers): with a
     * `block_for > 0` driver config, a child parked inside the extension's
     * blocking `next()` may not observe SIGTERM promptly; run auto-scaling
     * with `block_for=0` (the default) so released children exit promptly.
     * A child that outlives the grace period is escalated to SIGKILL
     * ({@see STOP_ESCALATION_SECONDS}).
     *
     * Requires ext-posix for `posix_kill`, which ships alongside ext-pcntl
     * on the platforms the forking path supports (same requirement as
     * Laravel Horizon); without it the fleet is left as is.
     *
     * @param  array<int, ChildSlot>  $slots
     */
    private function releaseIdleSlots(array &$slots, int $entryIndex, int $count, float $now): void
    {
        if (! function_exists('posix_kill')) {
            return;
        }

        $victims = [];
        foreach ($slots as $index => $slot) {
            if ($slot['entry'] === $entryIndex && $slot['process']->isRunning()) {
                $victims[] = $index;
            }
        }

        rsort($victims);
        $victims = array_slice($victims, 0, $count);

        foreach ($victims as $index) {
            $slot = $slots[$index];
            $pid = $slot['process']->getPid();
            if ($pid === null || ! posix_kill($pid, SIGTERM)) {
                // The child exited between the poll and the signal: leave
                // the slot to the regular clean-exit path.
                continue;
            }

            $slot['stopping'] = true;
            $slot['stoppingAt'] = $now;
            $slots[$index] = $slot;
        }
    }

    /**
     * Sends SIGKILL to one running child (escalation path).
     */
    private function killSlot(Process $process): void
    {
        $pid = $process->getPid();
        if ($pid !== null && function_exists('posix_kill')) {
            posix_kill($pid, SIGKILL);
        }
    }

    /**
     * Number of running children of one plan entry (children waiting out a
     * crash backoff are not consuming yet and do not count as live).
     *
     * @param  array<int, ChildSlot>  $slots
     */
    private function liveCount(array $slots, int $entryIndex): int
    {
        $live = 0;
        foreach ($slots as $slot) {
            if ($slot['entry'] === $entryIndex && $slot['process']->isRunning()) {
                $live++;
            }
        }

        return $live;
    }

    /**
     * Handles one dead child slot: recycles a clean exit immediately (crash
     * budget reset), restarts a crashed child once its backoff window has
     * elapsed, or schedules the next backoff. Returns EXIT_MAX_RESTARTS when
     * a crashed child exhausted its restart budget (all children stopped),
     * null when supervision continues.
     *
     * @param  array<int, ChildSlot>  $slots
     */
    private function superviseDeadSlot(int $index, array &$slots, float $now): ?int
    {
        $slot = $slots[$index];

        if ($this->isCleanExit($slot['process'])) {
            // Planned recycling (e.g. --max-jobs reached): reset the
            // crash budget and restart immediately, without backoff.
            $slot['restarts'] = 0;
            $slots[$index] = $this->restartSlot($index, $slot);
        } elseif ($slot['restartAt'] !== 0.0) {
            // A restart is already scheduled for this worker: wait for
            // its backoff window to elapse, then restart it. The other
            // children keep being supervised in the meantime.
            if ($now >= $slot['restartAt']) {
                $slot['restartAt'] = 0.0;
                $slots[$index] = $this->restartSlot($index, $slot);
            }
        } elseif (! $this->shouldRestart($slot['restarts'])) {
            $this->stopAllSlots($slots);

            return self::EXIT_MAX_RESTARTS;
        } else {
            // Schedule the restart with its backoff; the loop keeps
            // polling the other children meanwhile (non-blocking backoff).
            $slot['restartAt'] = $now + $this->backoffSeconds($slot['restarts']);
            $slot['restarts']++;
            $slots[$index] = $slot;
        }

        return null;
    }

    /**
     * Restarts one slot's child in place: same worker index and plan entry,
     * fresh process.
     *
     * @param  ChildSlot  $slot
     * @return ChildSlot
     */
    private function restartSlot(int $index, array $slot): array
    {
        $slot['process'] = $this->startProcess($index, $this->childCommand($index, $this->plan[$slot['entry']]));

        return $slot;
    }

    /**
     * Stop all child processes gracefully (the supervisor's own shutdown
     * path).
     *
     * @param  array<int, ChildSlot>  $slots
     */
    private function stopAllSlots(array $slots): void
    {
        foreach ($slots as $slot) {
            if ($slot['process']->isRunning()) {
                $slot['process']->stop(10, SIGTERM);
            }
        }
    }

    /**
     * @param  list<string>  $command  The child command for this worker index.
     */
    private function startProcess(int $workerIndex, array $command): Process
    {
        if ($this->processFactory !== null) {
            $process = ($this->processFactory)($workerIndex);
        } else {
            $process = new Process(
                $command,
                null,
                $this->workerEnvironment($workerIndex),
            );
        }
        $process->start();

        return $process;
    }
}
