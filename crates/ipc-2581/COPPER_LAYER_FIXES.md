# Copper Layer Rendering - Remaining Issues

## Current Status
- Board outlines render correctly with smooth arcs
- Traces have geometric errors - extend outside board edges on testcase11

## Root Cause Analysis
The issue is in trace stroking:
1. We tessellate arcs into line segments (32 segments)
2. We create rectangles for each segment  
3. We add circles at endpoints for round caps
4. This creates **miter joins** between segments that overshoot on curves

## Solution
Use **lyon_algorithms** for proper stroke expansion:
- lyon has production-grade path offsetting
- Handles round joins/caps correctly
- No geometric overshoot on curves

## Implementation Plan
1. Build lyon_path::Path from trace line segments
2. Use lyon_algorithms to expand/stroke the path
3. Convert result to geo::Polygon
4. Maintain performance (<2s per board)
