INSERT INTO clickdoom.cpu_state (batch_id, icount, pc, regs, halted, halt_reason, exit_code)
SELECT batch_id, icount, pc, regs, halted, halt_reason, exit_code
FROM clickdoom.batch_commit
WHERE batch_id = 7