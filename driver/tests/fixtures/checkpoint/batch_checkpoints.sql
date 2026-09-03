SELECT concat(toString(icount), '	', lpad(lower(hex(pc)), 8, '0'), '	', lpad(lower(hex(reghash)), 16, '0'))
FROM (
    SELECT cp_icount[n] AS icount, cp_pc[n] AS pc,
           arraySlice(cp_regs, (n - 1) * 31 + 1, 31) AS regs,
           xxHash64(concat(reinterpretAsFixedString(toUInt32(pc)), reinterpretAsFixedString(toUInt32(regs[1])), reinterpretAsFixedString(toUInt32(regs[2])), reinterpretAsFixedString(toUInt32(regs[3])), reinterpretAsFixedString(toUInt32(regs[4])), reinterpretAsFixedString(toUInt32(regs[5])), reinterpretAsFixedString(toUInt32(regs[6])), reinterpretAsFixedString(toUInt32(regs[7])), reinterpretAsFixedString(toUInt32(regs[8])), reinterpretAsFixedString(toUInt32(regs[9])), reinterpretAsFixedString(toUInt32(regs[10])), reinterpretAsFixedString(toUInt32(regs[11])), reinterpretAsFixedString(toUInt32(regs[12])), reinterpretAsFixedString(toUInt32(regs[13])), reinterpretAsFixedString(toUInt32(regs[14])), reinterpretAsFixedString(toUInt32(regs[15])), reinterpretAsFixedString(toUInt32(regs[16])), reinterpretAsFixedString(toUInt32(regs[17])), reinterpretAsFixedString(toUInt32(regs[18])), reinterpretAsFixedString(toUInt32(regs[19])), reinterpretAsFixedString(toUInt32(regs[20])), reinterpretAsFixedString(toUInt32(regs[21])), reinterpretAsFixedString(toUInt32(regs[22])), reinterpretAsFixedString(toUInt32(regs[23])), reinterpretAsFixedString(toUInt32(regs[24])), reinterpretAsFixedString(toUInt32(regs[25])), reinterpretAsFixedString(toUInt32(regs[26])), reinterpretAsFixedString(toUInt32(regs[27])), reinterpretAsFixedString(toUInt32(regs[28])), reinterpretAsFixedString(toUInt32(regs[29])), reinterpretAsFixedString(toUInt32(regs[30])), reinterpretAsFixedString(toUInt32(regs[31])))) AS reghash
    FROM (
        SELECT cp_icount, cp_pc, cp_regs, arrayJoin(arrayEnumerate(cp_icount)) AS n
        FROM testdb.batch_commit
        WHERE batch_id = 42
    )
)
ORDER BY icount