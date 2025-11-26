#!/usr/bin/env python3
"""
Quick PCB Auto-Placer
Optimizes component placement using simulated annealing
"""

import json
import random
import math
import copy
import sys
from dataclasses import dataclass, asdict
from typing import List, Dict, Tuple, Optional

@dataclass
class Component:
    """Represents a PCB component"""
    ref: str           # Reference designator (e.g., "R1", "D1")
    x: float           # X position (mm)
    y: float           # Y position (mm)
    width: float       # Component width (mm)
    height: float      # Component height (mm)
    rotation: float    # Rotation in degrees
    nets: List[str]    # Connected nets
    is_fixed: bool = False  # Don't move this component
    thermal_power: float = 0.0  # Power dissipation in watts

@dataclass
class Net:
    """Represents an electrical connection"""
    name: str
    components: List[str]  # List of component refs

@dataclass
class Board:
    """Represents the PCB board"""
    width: float
    height: float
    components: List[Component]
    nets: List[Net]

class AutoPlacer:
    """PCB auto-placement optimizer"""
    
    def __init__(self, config: Dict):
        self.config = config
        self.weights = config.get('weights', {
            'wire_length': 1.0,
            'overlap': 100.0,
            'off_board': 50.0,
            'thermal': 0.5,
        })
    
    def score_layout(self, board: Board) -> float:
        """Calculate quality score for layout (higher is better)"""
        score = 0.0
        
        # 1. Wire length - minimize total connection length
        wire_score = -self.calculate_total_wire_length(board)
        score += self.weights['wire_length'] * wire_score
        
        # 2. Overlaps - heavily penalize overlapping components
        overlap_score = -self.count_overlaps(board)
        score += self.weights['overlap'] * overlap_score
        
        # 3. Off-board - penalize components outside board
        off_board_score = -self.count_off_board(board)
        score += self.weights['off_board'] * off_board_score
        
        # 4. Thermal - separate hot components
        thermal_score = -self.calculate_thermal_violations(board)
        score += self.weights['thermal'] * thermal_score
        
        return score
    
    def calculate_total_wire_length(self, board: Board) -> float:
        """Calculate total wire length (Manhattan distance)"""
        total = 0.0
        
        for net in board.nets:
            if len(net.components) < 2:
                continue
            
            # Get component positions for this net
            positions = []
            for comp_ref in net.components:
                comp = self.find_component(board, comp_ref)
                if comp:
                    positions.append((comp.x, comp.y))
            
            # Calculate minimum spanning tree length (approximation)
            if len(positions) >= 2:
                # Simple approximation: sum distances from first component
                base = positions[0]
                for pos in positions[1:]:
                    dx = abs(pos[0] - base[0])
                    dy = abs(pos[1] - base[1])
                    total += dx + dy  # Manhattan distance
        
        return total
    
    def count_overlaps(self, board: Board) -> int:
        """Count number of overlapping component pairs"""
        overlaps = 0
        
        for i, comp1 in enumerate(board.components):
            for comp2 in board.components[i+1:]:
                if self.components_overlap(comp1, comp2):
                    overlaps += 1
        
        return overlaps
    
    def components_overlap(self, c1: Component, c2: Component) -> bool:
        """Check if two components overlap"""
        # Add 0.5mm spacing requirement
        spacing = 0.5
        
        left1 = c1.x - c1.width/2 - spacing
        right1 = c1.x + c1.width/2 + spacing
        top1 = c1.y - c1.height/2 - spacing
        bottom1 = c1.y + c1.height/2 + spacing
        
        left2 = c2.x - c2.width/2 - spacing
        right2 = c2.x + c2.width/2 + spacing
        top2 = c2.y - c2.height/2 - spacing
        bottom2 = c2.y + c2.height/2 + spacing
        
        # Check if rectangles overlap
        return not (right1 < left2 or right2 < left1 or 
                   bottom1 < top2 or bottom2 < top1)
    
    def count_off_board(self, board: Board) -> int:
        """Count components that are off the board"""
        count = 0
        margin = 2.0  # 2mm margin from edge
        
        for comp in board.components:
            if (comp.x - comp.width/2 < margin or
                comp.x + comp.width/2 > board.width - margin or
                comp.y - comp.height/2 < margin or
                comp.y + comp.height/2 > board.height - margin):
                count += 1
        
        return count
    
    def calculate_thermal_violations(self, board: Board) -> float:
        """Calculate thermal spacing violations"""
        violations = 0.0
        min_thermal_spacing = 5.0  # 5mm minimum for hot components
        
        hot_components = [c for c in board.components if c.thermal_power > 0.5]
        
        for i, comp1 in enumerate(hot_components):
            for comp2 in hot_components[i+1:]:
                distance = math.sqrt(
                    (comp1.x - comp2.x)**2 + (comp1.y - comp2.y)**2
                )
                if distance < min_thermal_spacing:
                    violations += (min_thermal_spacing - distance)
        
        return violations
    
    def find_component(self, board: Board, ref: str) -> Optional[Component]:
        """Find component by reference"""
        for comp in board.components:
            if comp.ref == ref:
                return comp
        return None
    
    def optimize(self, board: Board, iterations: int = 5000) -> Board:
        """Optimize layout using simulated annealing"""
        current = board
        best = copy.deepcopy(board)
        
        current_score = self.score_layout(current)
        best_score = current_score
        
        temperature = 100.0
        cooling_rate = 0.995
        min_temperature = 0.1
        
        print(f"Initial score: {current_score:.2f}")
        print(f"Optimizing with {iterations} iterations...")
        
        for i in range(iterations):
            # Generate neighbor solution
            neighbor = self.generate_neighbor(current)
            neighbor_score = self.score_layout(neighbor)
            
            # Calculate acceptance probability
            delta = neighbor_score - current_score
            
            if delta > 0:
                # Always accept improvements
                accept = True
            else:
                # Sometimes accept worse solutions (escape local minima)
                accept = random.random() < math.exp(delta / temperature)
            
            if accept:
                current = neighbor
                current_score = neighbor_score
                
                # Update best
                if current_score > best_score:
                    best = copy.deepcopy(current)
                    best_score = current_score
            
            # Cool down
            temperature *= cooling_rate
            
            # Progress update
            if i % 500 == 0:
                print(f"  Iteration {i}: Score = {current_score:.2f}, "
                      f"Best = {best_score:.2f}, Temp = {temperature:.2f}")
            
            if temperature < min_temperature:
                break
        
        print(f"Optimization complete!")
        print(f"Final score: {best_score:.2f}")
        print(f"Improvement: {best_score - self.score_layout(board):.2f}")
        
        return best
    
    def generate_neighbor(self, board: Board) -> Board:
        """Generate neighboring solution by making small random change"""
        new_board = copy.deepcopy(board)
        
        # Get non-fixed components
        movable = [c for c in new_board.components if not c.is_fixed]
        if not movable:
            return new_board
        
        # Pick random component
        comp = random.choice(movable)
        
        # Choose random modification
        action = random.choices(
            ['move', 'rotate', 'swap'],
            weights=[0.6, 0.2, 0.2]
        )[0]
        
        if action == 'move':
            # Move by small random amount (Gaussian distribution)
            std_dev = 3.0  # 3mm standard deviation
            comp.x += random.gauss(0, std_dev)
            comp.y += random.gauss(0, std_dev)
            
        elif action == 'rotate':
            # Rotate 90 degrees
            comp.rotation = (comp.rotation + 90) % 360
            comp.width, comp.height = comp.height, comp.width
            
        elif action == 'swap':
            # Swap positions with another component
            other = random.choice(movable)
            if other != comp:
                comp.x, other.x = other.x, comp.x
                comp.y, other.y = other.y, comp.y
        
        return new_board

def load_from_json(filename: str) -> Board:
    """Load board data from JSON file"""
    with open(filename) as f:
        data = json.load(f)
    
    components = [Component(**c) for c in data['components']]
    nets = [Net(**n) for n in data.get('nets', [])]
    
    return Board(
        width=data['board_width'],
        height=data['board_height'],
        components=components,
        nets=nets
    )

def save_to_json(board: Board, filename: str):
    """Save board data to JSON file"""
    data = {
        'board_width': board.width,
        'board_height': board.height,
        'components': [asdict(c) for c in board.components],
        'nets': [asdict(n) for n in board.nets]
    }
    
    with open(filename, 'w') as f:
        json.dump(data, f, indent=2)

def main():
    if len(sys.argv) < 3:
        print("Usage: python3 quick_autoplace.py <input.json> <output.json> [iterations]")
        print("\nExample:")
        print("  python3 quick_autoplace.py components.json optimized.json 10000")
        sys.exit(1)
    
    input_file = sys.argv[1]
    output_file = sys.argv[2]
    iterations = int(sys.argv[3]) if len(sys.argv) > 3 else 5000
    
    # Load board
    print(f"Loading board from {input_file}...")
    board = load_from_json(input_file)
    print(f"  Board: {board.width}mm x {board.height}mm")
    print(f"  Components: {len(board.components)}")
    print(f"  Nets: {len(board.nets)}")
    
    # Optimize
    config = {
        'weights': {
            'wire_length': 1.0,
            'overlap': 100.0,
            'off_board': 50.0,
            'thermal': 0.5,
        }
    }
    
    placer = AutoPlacer(config)
    optimized = placer.optimize(board, iterations=iterations)
    
    # Save result
    save_to_json(optimized, output_file)
    print(f"\nSaved optimized layout to {output_file}")
    
    # Print summary
    print("\nLayout Summary:")
    print(f"  Total wire length: {placer.calculate_total_wire_length(optimized):.2f}mm")
    print(f"  Overlaps: {placer.count_overlaps(optimized)}")
    print(f"  Off-board components: {placer.count_off_board(optimized)}")

if __name__ == "__main__":
    main()
